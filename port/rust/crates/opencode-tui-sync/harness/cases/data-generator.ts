// Chooses the next step of a generated data case: events in plausible lifecycles over a small universe of sessions,
// calls the TUI makes into the data layer, answers to whichever requests the layer has pending (late, out of order,
// failed), disconnects and reconnects, and time passing. Deterministic for a seed and the layer's behaviour.
import { Random } from "./random"

type Pending = { recorded: { method: string; url: string; body: string | null }; settled: boolean }
type Step = Record<string, unknown> & { op: string }

const DIRECTORY = "/project"
const LOCATION = { directory: DIRECTORY }
const OTHER = { directory: "/other" }
const PROJECT = "prj_main"
const SESSIONS = ["ses_a", "ses_b", "ses_c", "ses_d"]
const PARENT: Record<string, string | undefined> = { ses_b: "ses_a", ses_d: "ses_b" }
const AGENTS = ["build", "plan"]
const MODEL = { providerID: "openrouter", id: "deepseek/deepseek-v4.1-flash-1017", variant: "max" }
const TOKENS = { input: 10, output: 5, reasoning: 0, cache: { read: 0, write: 0 } }

export class Generator {
  random: Random
  private remaining: number
  private now = 1_000
  private counter = 0
  // Per session: the assistant message in progress, its open tools, inbox IDs, message IDs known.
  private assistant: Record<string, string | undefined> = {}
  private tools: Record<string, string[]> = {}
  private inbox: Record<string, string[]> = {}
  private messages: Record<string, string[]> = {}
  private permissions: Record<string, string[]> = {}
  private forms: string[] = []
  private shells: string[] = []
  private connected = true

  constructor(options: { seed: number; steps: number }) {
    this.random = new Random(options.seed)
    this.remaining = options.steps
  }

  private id(prefix: string) {
    this.counter++
    return `${prefix}_${String(this.counter).padStart(6, "0")}`
  }

  private session() {
    return this.random.pick(SESSIONS)
  }

  next(requests: Pending[], _store: unknown, status: string): Step | undefined {
    if (this.remaining-- <= 0) return undefined
    this.now += this.random.int(50)
    const pending = requests.map((request, index) => ({ request, index })).filter((entry) => !entry.request.settled)
    const roll = this.random.next()
    if (!this.connected && roll < 0.5) {
      this.connected = true
      return { op: "status", value: "connected" }
    }
    if (this.random.bool(0.06)) return this.retention()
    if (pending.length && roll < 0.38) return this.answer(pending)
    if (roll < 0.7) return { op: "events", events: Array.from({ length: 1 + this.random.int(3) }, () => this.event()) }
    if (roll < 0.9) return this.call()
    if (roll < 0.93 && status === "connected") {
      this.connected = false
      return { op: "status", value: this.random.pick(["reconnecting", "connecting"]) }
    }
    return { op: "advance", ms: this.random.pick([5, 10, 149, 150, 1000]) }
  }

  private retained = false

  // Session retention (created once, then navigated) and the context-usage selection.
  private retention(): Step {
    if (!this.retained) {
      this.retained = true
      return { op: "retain", limit: this.random.pick([0, 1, 2]) }
    }
    if (this.random.bool(0.3)) {
      const messages = this.messages[this.session()] ?? []
      return { op: "usage", session: this.session(), boundary: this.random.bool(0.3) ? this.random.pick([...messages, "msg_none"]) : null }
    }
    return {
      op: "view",
      current: this.random.bool(0.8) ? this.session() : null,
      keep: SESSIONS.filter(() => this.random.bool(0.2)),
    }
  }

  // ---- answers --------------------------------------------------------------------------------------------

  answer(pending: { request: Pending; index: number }[]): Step {
    // Mostly the oldest requests, sometimes a later one first (a response that overtakes another).
    const entry = this.random.bool(0.7) ? pending[0] : this.random.pick(pending)
    const { method, url } = entry.request.recorded
    this.lastBody = entry.request.recorded.body
    const roll = this.random.next()
    if (roll < 0.05) return { op: "fail", request: entry.index, error: { name: "TypeError", message: "Unable to connect. Is the computer able to access the url?" } }
    if (roll < 0.08) return this.json(entry.index, 500, { _tag: "UnknownError", message: "boom" })
    const parsed = new URL(url)
    return this.respond(entry.index, method, parsed.pathname, parsed.searchParams)
  }

  private json(index: number, status: number, body: unknown): Step {
    return { op: "respond", request: index, status, headers: [["content-type", "application/json"]], chunks: [JSON.stringify(body)] }
  }

  private empty(index: number): Step {
    return { op: "respond", request: index, status: 204, headers: [], chunks: [] }
  }

  private notFound(index: number, session: string): Step {
    return this.json(index, 404, { _tag: "SessionNotFoundError", sessionID: session, message: `Session not found: ${session}` })
  }

  private respond(index: number, method: string, path: string, query: URLSearchParams): Step {
    const location = { directory: query.get("location[directory]") ?? DIRECTORY }
    const segments = path.split("/").slice(2)
    const session = segments[1]
    if (method === "GET" && path === "/api/session/active")
      return this.json(index, 200, { data: Object.fromEntries(SESSIONS.filter(() => this.random.bool(0.3)).map((id) => [id, { type: "busy" }])) })
    if (location.directory === "/gone" && path !== "/api/session/active")
      return this.json(index, 404, { _tag: "LocationNotFoundError", directory: "/gone", message: "Location not found: /gone" })
    if (method === "GET" && path === "/api/location")
      return this.json(index, 200, { directory: location.directory, project: { id: PROJECT, worktree: location.directory } })
    if (method === "GET" && path === "/api/vcs") return this.json(index, 200, { location, data: { branch: this.random.pick(["main", "dev"]), default: "main" } })
    if (method === "GET" && path === "/api/project") return this.json(index, 200, this.projects())
    if (method === "GET" && path === "/api/session") {
      const parent = query.get("parentID")
      const children = SESSIONS.filter((id) => PARENT[id] === parent).map((id) => this.info(id))
      return this.json(index, 200, { data: children, cursor: this.random.bool(0.1) ? { next: "page_2" } : {} })
    }
    if (method === "POST" && path === "/api/session") {
      const body = JSON.parse(this.lastBody ?? "{}")
      if (this.random.bool(0.15)) return this.json(index, 503, { _tag: "ServiceUnavailableError", message: "unavailable" })
      // The server answers with the session at the location it was asked for.
      return this.json(index, 200, { data: { ...this.info(body.id ?? "ses_new"), title: "Server title", ...(body.location ? { location: body.location } : {}) } })
    }
    if (segments[0] === "session" && session && segments.length === 2 && method === "GET") {
      if (this.random.bool(0.1)) return this.notFound(index, session)
      return this.json(index, 200, { data: this.info(session) })
    }
    if (segments[2] === "message" && segments.length === 3)
      return this.json(index, 200, {
        data: this.history(session, query.get("cursor") !== null).reverse(),
        cursor: this.random.bool(0.4) ? { next: `cursor_${this.counter}` } : {},
      })
    if (segments[2] === "message" && segments.length === 4) {
      if (this.random.bool(0.2)) return this.json(index, 404, { _tag: "MessageNotFoundError", sessionID: session, messageID: segments[3], message: "not found" })
      return this.json(index, 200, { data: { id: segments[3], type: "model-switched", model: MODEL, time: { created: this.now } } })
    }
    if (segments[2] === "inbox") return this.json(index, 200, { data: this.inboxItems(session) })
    if (segments[2] === "permission" && method === "GET") return this.json(index, 200, { data: (this.permissions[session] ?? []).map((id) => this.permission(session, id)) })
    if (segments[2] === "permission" && method === "POST") {
      if (this.random.bool(0.3)) return this.json(index, 404, { _tag: "PermissionNotFoundError", requestID: segments[3], message: "gone" })
      return this.empty(index)
    }
    if (segments[2] === "form" && method === "GET") return this.json(index, 200, { data: this.forms.slice(0, 2).map((id) => ({ id, sessionID: session, title: "Form" })) })
    if (segments[2] === "form") {
      if (this.random.bool(0.3)) return this.json(index, 404, { _tag: "FormNotFoundError", id: segments[3], message: "gone" })
      return this.empty(index)
    }
    if (path === "/api/form") return this.json(index, 200, { location, data: this.forms.map((id) => ({ id, sessionID: this.random.pick(["global", "ses_a"]), title: "Form" })) })
    if (segments[2] === "prompt") {
      const body = JSON.parse(this.lastBody ?? "{}")
      if (this.random.bool(0.15)) return this.json(index, 409, { _tag: "ConflictError", message: "busy" })
      return this.json(index, 200, { data: { id: body.id, sessionID: session, type: "user", delivery: body.delivery ?? "steer", time: { created: this.now }, payload: { text: body.text } } })
    }
    if (segments[2] === "compact") {
      const id = this.random.bool(0.5) ? this.id("msg") : (JSON.parse(this.lastBody ?? "{}").id ?? this.id("msg"))
      return this.json(index, 200, { data: { id, sessionID: session, type: "compaction", delivery: "steer", time: { created: this.now }, payload: {} } })
    }
    if (segments[2] === "model" || segments[2] === "agent") return this.empty(index)
    if (path === "/api/config") return this.json(index, 200, [{ type: "document", info: { model: "openrouter/x" } }])
    if (path === "/api/mcp/resource") return this.json(index, 200, { location, data: { resources: [{ uri: "r://1" }] } })
    if (path === "/api/shell") return this.json(index, 200, { location, data: this.shells.map((id) => ({ id, command: "ls", status: "running", metadata: { sessionID: "ses_a" } })) })
    if (path === "/api/experimental/usage/limits") return this.json(index, 200, { location, data: this.random.bool() ? null : { go: { used: 1 } } })
    if (path === "/api/permission/saved") return this.json(index, 200, { data: [{ id: "perm_1", action: "read" }] })
    const catalog: Record<string, unknown[]> = {
      "/api/agent": AGENTS.map((id) => ({ id, mode: "primary" })),
      "/api/command": [{ id: "init", name: "init" }],
      "/api/integration": [{ id: "openrouter", connections: [{ type: "credential", id: "cred_1" }, { type: "credential", id: "cred_2" }] }],
      "/api/mcp": [],
      "/api/model": [{ id: MODEL.id, providerID: MODEL.providerID, variants: [] }],
      "/api/provider": [{ id: "openrouter", name: "OpenRouter" }],
      "/api/reference": [],
      "/api/skill": [{ id: "skill_1" }],
      "/api/websearch/provider": [],
    }
    if (path in catalog) {
      const answered = this.random.bool(0.2) ? OTHER : location
      return this.json(index, 200, { location: answered, data: catalog[path] })
    }
    return this.json(index, 200, {})
  }

  private lastBody: string | null = null

  private projects() {
    return [
      { id: PROJECT, worktree: DIRECTORY, canonical: DIRECTORY, time: { created: 1, active: this.now } },
      { id: "prj_other", worktree: "/other", time: { created: 1, active: 5, ...(this.random.bool() ? { archived: 3 } : {}) } },
    ]
  }

  private info(session: string) {
    return {
      id: session,
      ...(PARENT[session] ? { parentID: PARENT[session] } : {}),
      projectID: this.random.bool(0.8) ? PROJECT : "global",
      agent: this.random.pick(AGENTS),
      model: MODEL,
      cost: this.random.int(100) / 100,
      tokens: TOKENS,
      time: { created: 1, updated: this.now - this.random.int(500) },
      title: `Title ${session}`,
      location: LOCATION,
    }
  }

  private history(session: string, older: boolean) {
    const out: unknown[] = []
    const known = this.messages[session] ?? []
    const ids = older ? known.slice(0, 2).map((id) => `${id}_old`) : known.slice(-3)
    for (const id of ids)
      out.push(
        this.random.bool()
          ? { id, type: "user", text: "hello", time: { created: this.now - 100 } }
          : {
              id,
              type: "assistant",
              agent: "build",
              model: MODEL,
              content: [
                { type: "text", text: "answer" },
                { type: "tool", id: `call_${id}`, name: "read", time: { created: 1 }, state: { status: this.random.pick(["running", "completed"]), input: {}, metadata: {} } },
              ],
              time: { created: this.now - 90, ...(this.random.bool() ? { completed: this.now - 80 } : {}) },
            },
      )
    for (const id of this.inbox[session] ?? []) if (this.random.bool(0.3)) out.push({ id, type: "user", text: "queued", time: { created: this.now } })
    return out
  }

  private inboxItems(session: string) {
    return (this.inbox[session] ?? [])
      .filter(() => this.random.bool(0.6))
      .map((id) => ({ id, sessionID: session, type: this.random.pick(["user", "user", "compaction"]), delivery: this.random.pick(["steer", "queue"]), time: { created: this.now }, payload: { text: "pending" } }))
  }

  private permission(session: string, id: string) {
    return { id, sessionID: session, action: "bash", resources: ["ls"], source: { type: "tool", id: "call_1" } }
  }

  // ---- events ---------------------------------------------------------------------------------------------

  private base(type: string, data: Record<string, unknown>, extra: Record<string, unknown> = {}) {
    return { id: this.id("evt"), created: this.now, type, data, ...extra }
  }

  event(): Record<string, unknown> {
    const session = this.session()
    const roll = this.random.int(30)
    const assistant = this.assistant[session]
    switch (roll) {
      case 0:
        return this.base("server.connected", {})
      case 1:
        return this.base("session.created", { sessionID: session, projectID: PROJECT, location: LOCATION, ...(PARENT[session] ? { parentID: PARENT[session] } : {}) })
      case 2: {
        const kind = this.random.pick(["renamed", "permissions", "metadata.updated", "archived", "unarchived", "usage.updated", "viewed", "revert.staged", "revert.cleared"])
        const data: Record<string, unknown> = { sessionID: session }
        if (kind === "renamed") data.title = `Renamed ${this.counter}`
        if (kind === "permissions") data.permissions = [{ action: "read", resource: "*", effect: "allow" }]
        if (kind === "metadata.updated") data.metadata = this.random.bool() ? { goal: "x", id: "m" } : { other: 1 }
        if (kind === "usage.updated") Object.assign(data, { cost: 1.5, tokens: TOKENS })
        if (kind === "revert.staged") data.revert = { messageID: (this.messages[session] ?? ["msg_000001"])[0] }
        return this.base(`session.${kind}`, data)
      }
      case 3:
        return this.base("session.agent.selected", { sessionID: session, agent: this.random.pick(AGENTS) })
      case 4:
        // The server always names the previous model (session.ts), so the store never falls back to its own.
        return this.base("session.model.selected", { sessionID: session, model: this.random.bool() ? { ...MODEL, variant: this.random.pick(["max", "low"]) } : { providerID: "openrouter", id: "other" }, previous: MODEL })
      case 5: {
        const inbox = this.id("msg")
        ;(this.inbox[session] ??= []).push(inbox)
        const type = this.random.pick(["user", "user", "synthetic", "compaction"])
        return this.base("session.inbox.enqueued", { sessionID: session, inboxID: inbox, item: { type, delivery: this.random.pick(["steer", "queue"]), payload: type === "compaction" ? {} : { text: `prompt ${inbox}` } } })
      }
      case 6: {
        const inbox = this.random.pick(this.inbox[session] ?? [this.id("msg")])
        ;(this.messages[session] ??= []).push(inbox)
        return this.base("session.inbox.delivered", { sessionID: session, inboxID: inbox })
      }
      case 7: {
        const inbox = this.random.pick(this.inbox[session] ?? [this.id("msg")])
        return this.random.bool()
          ? this.base("session.inbox.cancelled", { sessionID: session, inboxID: inbox })
          : this.base("session.inbox.delivery.changed", { sessionID: session, inboxID: inbox, delivery: this.random.pick(["steer", "queue"]) })
      }
      case 8:
        return this.base("session.execution.started", { sessionID: session })
      case 9: {
        const kind = this.random.pick(["succeeded", "failed", "interrupted"])
        return this.base(`session.execution.${kind}`, { sessionID: session, ...(kind === "failed" ? { error: { type: "unknown", message: "x" } } : {}), ...(kind === "interrupted" ? { reason: this.random.pick(["user", "shutdown"]) } : {}) })
      }
      case 10:
      case 11: {
        const id = this.random.bool(0.3) && assistant ? assistant : this.id("msg")
        this.assistant[session] = id
        ;(this.messages[session] ??= []).push(id)
        return this.base("session.step.started", { sessionID: session, assistantMessageID: id, agent: "build", model: MODEL, started: this.now, ...(this.random.bool() ? { snapshot: "snap_1" } : {}) })
      }
      case 12:
        return this.base("session.step.streamed", { sessionID: session, assistantMessageID: assistant ?? "msg_none" })
      case 13:
        return this.random.bool()
          ? this.base("session.step.ended", { sessionID: session, assistantMessageID: assistant ?? "msg_none", finish: "stop", cost: 0.01, tokens: TOKENS, ...(this.random.bool() ? { snapshot: "snap_2" } : {}) })
          : this.base("session.step.failed", { sessionID: session, assistantMessageID: assistant ?? "msg_none", error: { type: this.random.pick(["provider.budget-limit", "unknown"]), message: "no" } }, { location: LOCATION })
      case 14:
        return this.base("session.text.started", { sessionID: session, assistantMessageID: assistant ?? "msg_none" })
      case 15:
      case 16:
        return this.base("session.text.delta", { sessionID: session, assistantMessageID: assistant ?? "msg_none", delta: this.random.pick(["Hel", "lo ", "wörld 😀", "\n"]) })
      case 17:
        return this.base("session.text.ended", { sessionID: session, assistantMessageID: assistant ?? "msg_none", text: "Hello wörld" })
      case 18: {
        const kind = this.random.pick(["started", "delta", "ended"])
        return this.base(`session.reasoning.${kind}`, { sessionID: session, assistantMessageID: assistant ?? "msg_none", ...(kind === "delta" ? { delta: "think " } : {}), ...(kind === "ended" ? { text: "thought", ...(this.random.bool() ? { native: { sig: "s" } } : {}) } : {}) })
      }
      case 19:
      case 20: {
        const tools = (this.tools[session] ??= [])
        const kind = this.random.pick(["input.started", "input.delta", "input.ended", "called", "progress", "success", "failed"])
        if (kind === "input.started") tools.push(this.id("call"))
        const id = kind === "input.started" ? tools.at(-1)! : this.random.pick(tools.length ? tools : ["call_none"])
        const data: Record<string, unknown> = { sessionID: session, assistantMessageID: assistant ?? "msg_none", id }
        if (kind === "input.started") data.name = "bash"
        if (kind === "input.delta") data.delta = '{"cmd":'
        if (kind === "input.ended") data.text = '{"cmd":"ls"}'
        if (kind === "called") Object.assign(data, { input: { cmd: "ls" }, executed: this.random.bool() })
        if (kind === "progress") data.metadata = { output: "partial" }
        if (kind === "success") Object.assign(data, { metadata: { exit: 0 }, content: [{ type: "text", text: "ok" }, { type: "file", uri: "data:image/png;base64,AAAA", mime: "image/png" }], executed: true, ...(this.random.bool() ? { native: { n: 1 } } : {}) })
        if (kind === "failed") Object.assign(data, { error: { type: "tool.execution", message: "bad" }, metadata: {} })
        return this.base(`session.tool.${kind}`, data)
      }
      case 21:
        return this.base("session.retry.scheduled", { sessionID: session, assistantMessageID: assistant ?? "msg_none", attempt: 1, at: this.now + 1000, error: { type: "rate", message: "slow" } })
      case 22: {
        const kind = this.random.pick(["started", "delta", "ended", "failed"])
        const input = this.random.bool() ? this.random.pick(this.inbox[session] ?? [this.id("msg")]) : undefined
        const data: Record<string, unknown> = { sessionID: session, reason: "manual" }
        if ((kind === "started" || kind === "failed") && input) data.inputID = input
        if (kind === "delta") data.text = "sum"
        if (kind === "ended") Object.assign(data, { text: "summary", model: MODEL, cost: 0.1, tokens: TOKENS })
        if (kind === "failed") Object.assign(data, { error: { type: "unknown", message: "x" } })
        return this.base(`session.compaction.${kind}`, data)
      }
      case 23: {
        const to = this.random.pick(this.messages[session] ?? ["msg_000000"])
        return this.base("session.revert.committed", { sessionID: session, to })
      }
      case 24: {
        const shell = this.random.bool() || !this.shells.length ? this.id("sh") : this.random.pick(this.shells)
        if (!this.shells.includes(shell)) this.shells.push(shell)
        return this.random.bool()
          ? this.base("session.shell.started", { sessionID: session, shell: { id: shell, command: "ls", status: "running", metadata: { background: this.random.bool() } } }, { metadata: { m: 1 } })
          : this.base("session.shell.ended", { sessionID: session, shell: { id: shell, command: "ls", status: "exited", exit: 0, metadata: {} }, output: "out" })
      }
      case 25:
        return this.random.bool()
          ? this.base("session.synthetic", { sessionID: session, text: "synthetic", description: "d", metadata: { a: 1 } })
          : this.base("session.instructions.updated", { sessionID: session, delta: { "AGENTS.md": "x" }, ...(this.random.bool() ? { text: "instructions" } : {}) }, { metadata: { k: 1 } })
      case 26: {
        const ids = (this.permissions[session] ??= [])
        if (this.random.bool() || !ids.length) {
          const id = this.id("per")
          ids.push(id)
          return this.base("permission.asked", this.permission(session, id))
        }
        return this.base("permission.replied", { sessionID: session, requestID: this.random.pick(ids) })
      }
      case 27: {
        if (this.random.bool() || !this.forms.length) {
          const id = this.id("frm")
          this.forms.push(id)
          return this.base("form.created", { form: { id, sessionID: this.random.pick(["global", session]), title: "Q" } }, { location: LOCATION })
        }
        return this.base(this.random.pick(["form.replied", "form.cancelled"]), { id: this.random.pick(this.forms), sessionID: this.random.pick(["global", session]) }, { location: LOCATION })
      }
      case 28: {
        const type = this.random.pick([
          "provider.updated", "model.updated", "agent.updated", "command.updated", "skill.updated", "vcs.branch.updated", "reference.updated",
          "integration.updated", "config.updated", "websearch.updated", "mcp.status.changed", "mcp.status.changed", "mcp.resources.changed",
          "shell.created", "shell.exited", "credential.updated", "credential.switched",
        ])
        const data: Record<string, unknown> = {}
        if (type === "shell.created") {
          const id = this.id("sh")
          this.shells.push(id)
          data.info = { id, command: "sleep", status: "running", metadata: { sessionID: session } }
        }
        if (type === "shell.exited") data.id = this.random.pick(this.shells.length ? this.shells : ["sh_none"])
        if (type === "credential.switched") Object.assign(data, { integrationID: "openrouter", credentialID: "cred_2" })
        if (type === "credential.updated") data.integrationID = "openrouter"
        return this.base(type, data, type.startsWith("credential") ? {} : { location: this.random.bool(0.85) ? LOCATION : OTHER })
      }
      default:
        return this.random.bool()
          ? this.base("session.moved", { sessionID: session, location: OTHER, projectID: "prj_other", ...(this.random.bool() ? { subpath: "sub" } : {}) })
          : this.random.bool()
            ? this.base("project.updated", { ...this.projects()[0], name: `name ${this.counter}` })
            : this.random.bool()
              ? this.base("worktree.resolved", { projectID: "prj_new", directory: DIRECTORY, previous: PROJECT, ...(this.random.bool() ? { adopted: ["global"] } : {}) })
              : this.base("session.deleted", { sessionID: session })
    }
  }

  // ---- calls ----------------------------------------------------------------------------------------------

  private call(): Step {
    const session = this.session()
    const roll = this.random.int(24)
    const call = (path: string, ...args: unknown[]) => ({ op: "call", path, args })
    switch (roll) {
      case 0:
        return call("session.sync", session, ...(this.random.bool(0.3) ? [{ children: true }] : []))
      case 1:
      case 2:
        return call("session.message.sync", session)
      case 3:
        return call("session.pending.sync", session)
      case 4:
        return call("session.message.loadMore", session, ...(this.random.bool(0.3) ? [{ all: true }] : []))
      case 5:
        return call("session.permission.sync", session)
      case 6:
        return call("session.form.sync", this.random.pick(["global", session]))
      case 7:
        return call(this.random.pick(["project.sync", "location.sync", "location.syncInfo", "location.vcs.sync", "location.model.sync", "location.provider.sync", "location.config.sync", "location.mcp.server.sync", "location.usage.sync", "location.websearch.refresh", "shell.sync"]))
      case 8: {
        const id = this.random.bool(0.5) ? this.id("ses_new") : undefined
        return call("session.create", { ...(id ? { id } : {}), agent: "build", model: MODEL, ...(this.random.bool() ? { title: "New" } : {}) })
      }
      case 9:
      case 10: {
        const id = this.id("msg")
        ;(this.inbox[session] ??= []).push(id)
        return call("session.prompt", { sessionID: session, ...(this.random.bool(0.8) ? { id } : {}), text: "hi there", ...(this.random.bool() ? { delivery: "queue" } : {}), ...(this.random.bool(0.3) ? { files: [{ uri: "data:text/plain;base64,aGk=", name: "a.txt" }] } : {}) })
      }
      case 11:
        return call("session.compact", { sessionID: session, ...(this.random.bool(0.3) ? { model: MODEL } : {}) })
      case 12:
        return call("session.evict", session)
      case 13:
        return call("session.remember", this.info(session))
      case 14:
        return call(this.random.pick(["session.forget", "session.invalidate", "session.message.invalidate", "session.pending.invalidate"]), session)
      case 15:
        return call("session.setStatus", session, this.random.pick(["running", "idle"]))
      case 16: {
        const ids = this.permissions[session] ?? ["per_none"]
        return call("session.permission.reply", { sessionID: session, requestID: this.random.pick(ids), reply: "once" })
      }
      case 17:
        return call(this.random.pick(["session.form.reply", "session.form.cancel"]), { sessionID: this.random.pick(["global", session]), formID: this.random.pick(this.forms.length ? this.forms : ["frm_none"]), ...(this.random.bool() ? { answers: { a: 1 } } : {}) }, ...(this.random.bool() ? [LOCATION] : []))
      case 18:
        return call(this.random.pick(["location.invalidate", "project.invalidate"]))
      default: {
        // Reads: their values are compared too.
        const reads = [
          ["session.list"], ["session.get", session], ["session.missing", session], ["session.root", session], ["session.family", session],
          ["session.cost", session], ["session.status", session], ["session.active"], ["session.input.list", session],
          ["session.pending.list", session], ["session.pending.status", session, this.random.pick(this.inbox[session] ?? ["msg_none"])],
          ["session.message.list", session], ["session.message.more", session], ["session.message.loading", session],
          ["session.message.get", session, this.random.pick(this.messages[session] ?? ["msg_none"])], ["session.permission.list", session],
          ["session.permission.sessions"], ["session.form.list", "global", LOCATION], ["session.form.sessions"], ["project.list"],
          ["shell.list"], ["shell.listBySession", session], ["location.info"], ["location.default"], ["location.model.list"],
          ["location.vcs.info"], ["session.creating", session],
        ]
        const [path, ...args] = this.random.pick(reads)
        return call(path as string, ...args)
      }
    }
  }
}
