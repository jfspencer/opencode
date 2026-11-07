// Session view: how stored history renders when a session is opened. The histories are imported through the
// API (fixed timestamps), then opened from the session list, so every message type and tool state can be shown,
// including ones the scripted model cannot produce live (patches, todos, diagnostics, images, notices).
import { rmSync } from "node:fs"
import path from "node:path"
import { MODEL_ID } from "../lib/model"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

const T0 = Date.UTC(2026, 0, 5, 10, 0, 0)
const MODEL = { providerID: "openrouter", id: MODEL_ID, variant: "max" }
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const at = (n: number) => T0 + n * 1000
const tokens = (input: number, output: number) => ({ input, output, reasoning: 0, cache: { read: 0, write: 0 } })

let counter = 0
const mid = () => `msg_00000000000000conformance${String(++counter).padStart(3, "0")}`

const user = (n: number, text: string, extra: Record<string, unknown> = {}) => ({
  id: mid(),
  type: "user",
  text,
  time: { created: at(n) },
  ...extra,
})

const assistant = (n: number, content: unknown[], extra: Record<string, unknown> = {}) => ({
  id: mid(),
  type: "assistant",
  agent: "build",
  model: MODEL,
  content,
  finish: "stop",
  tokens: tokens(1000, 50),
  time: { created: at(n), completed: at(n) + 1500 },
  ...extra,
})

const tool = (id: string, name: string, n: number, state: Record<string, unknown>) => ({
  type: "tool",
  id,
  name,
  state,
  time: { created: at(n), ran: at(n) + 100, completed: at(n) + 500 },
})

const done = (input: Record<string, unknown>, text: string, metadata?: Record<string, unknown>) => ({
  status: "completed",
  input,
  content: [{ type: "text", text }],
  ...(metadata ? { metadata } : {}),
})

const failed = (input: Record<string, unknown>, message: string, metadata?: Record<string, unknown>) => ({
  status: "error",
  input,
  error: { type: "tool", message },
  ...(metadata ? { metadata } : {}),
})

/** Import a session with the given history, then open it from the session list. */
async function open(
  t: TuiCtx,
  title: string,
  messages: unknown[],
  info: Record<string, unknown> = {},
  allProjects = false,
) {
  const id = `ses_0000000000000conformance${String(++counter).padStart(3, "0")}`
  await t.http("POST", "/api/experimental/session/import", {
    info: {
      id,
      projectID: "0000000000000000000000000000000000000000",
      title,
      cost: 0,
      tokens: tokens(0, 0),
      time: { created: T0, updated: T0 },
      location: { directory: t.project },
      ...info,
    },
    location: info.location ?? { directory: t.project },
    messages,
  })
  await t.press("x", { ctrl: true })
  await t.press("l")
  if (allProjects) await t.press("a", { ctrl: true })
  await t.waitFor(title)
  await t.type(title)
  await t.enter()
  await t.waitFor(new RegExp(`^\\s*${title}`))
  return id
}

function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  tuiScenario(name, options, async (t) => {
    const started = Date.now()
    counter = 0
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

const PATCH = "@@ -1,3 +1,3 @@\n export const a = 1\n-export const b = 2\n+export const b = 20\n export const c = 3\n"

scenario("patches, todos and diagnostics from history", { git: true, height: 80, tui: { session: { grouping: "none" } } }, async (t) => {
  await open(t, "Patch history", [
    user(1, "Refactor the module"),
    assistant(2, [
      tool("call_p1", "apply_patch", 2, {
        status: "completed",
        input: { patchText: "*** Begin Patch\n*** Update File: src/a.ts\n*** End Patch" },
        content: [{ type: "text", text: "Applied." }],
        metadata: {
          files: [
            { type: "update", relativePath: "src/a.ts", filePath: "src/a.ts", patch: PATCH, additions: 1, deletions: 1 },
            { status: "added", file: "src/new.ts", patch: "@@ -0,0 +1 @@\n+export const n = 1\n", additions: 1, deletions: 0 },
            { type: "delete", relativePath: "src/old.ts", filePath: "src/old.ts", patch: "", additions: 0, deletions: 3 },
          ],
        },
      }),
      tool("call_p2", "apply_patch", 2, {
        status: "completed",
        input: { patchText: "*** Begin Patch\n*** Add File: src/b.ts\n*** End Patch" },
        content: [{ type: "text", text: "Applied." }],
        metadata: { applied: [{ type: "add", resource: "src/b.ts" }, { type: "delete", resource: "src/c.ts" }] },
      }),
      tool("call_p3", "apply_patch", 2, failed({ patchText: "*** Begin Patch\n*** Update File: src/x.ts\n*** End Patch" }, "Context not found")),
      tool("call_p4", "apply_patch", 2, failed({ patchText: "*** Update File: a\n*** Update File: b" }, "Two files failed")),
      tool("call_t2", "todowrite", 2, done({ todos: [] }, "cleared")),
      tool("call_t3", "todowrite", 2, failed({ todos: "nope" }, "Invalid todos")),
      tool("call_t1", "todowrite", 2, done(
        {
          todos: [
            { content: "Write the parser", status: "completed" },
            { content: "Wire up the CLI", status: "in_progress" },
            { content: "Document flags", status: "pending" },
            { content: "Drop legacy mode", status: "cancelled" },
          ],
        },
        "4 todos",
      )),
      tool("call_w1", "write", 2, done({ path: "src/typed.ts", content: "export const x: number = 'one'\n" }, "Wrote.", {
        diagnostics: {
          "src/typed.ts": [
            { severity: 1, range: { start: { line: 0, character: 25 } }, message: "Type 'string' is not assignable to type 'number'." },
            { severity: 2, range: { start: { line: 0, character: 0 } }, message: "A warning that is not shown" },
          ],
        },
      })),
      tool("call_e1", "edit", 2, done({ path: "src/a.ts", oldString: "2", newString: "20" }, "Edited.", {
        files: [{ type: "update", relativePath: "src/a.ts", filePath: "src/a.ts", patch: PATCH, additions: 1, deletions: 1 }],
        diagnostics: { "src/a.ts": [{ severity: 1, range: { start: { line: 1, character: 13 } }, message: "Unused export" }] },
      })),
      { type: "text", text: "Refactored." },
    ]),
  ])
  await t.waitFor("Refactored.")
  await t.frame("bottom")
  await t.press("g", { ctrl: true })
  await t.frame("top")
  // Wide enough for the sidebar, which lists the latest todos.
  await t.resize(160, 40)
  await t.waitFor("Wire up the CLI")
  await t.frame("sidebar-todos")
})

scenario("tool states and legacy tool names from history", { height: 60, tui: { session: { grouping: "none" } } }, async (t) => {
  await open(t, "Tool history", [
    user(1, "Show every state"),
    assistant(2, [
      tool("call_1", "read", 2, done({ path: "README.md" }, "# Demo", { loaded: ["AGENTS.md", "docs/AGENTS.md"] })),
      tool("call_2", "bash", 2, done({ command: "make build", workdir: "/" }, "built")),
      tool("call_3", "shell", 2, done({ command: "npm run dev", background: true }, "started", { shellID: "sh_00000000000000conformance01", status: "running" })),
      tool("call_4", "task", 2, done({ subagent_type: "explore", description: "Look around", prompt: "Look" }, "Looked.", { sessionID: "ses_00000000000000conformanceXX", status: "running" })),
      tool("call_5", "question", 2, done(
        { questions: [{ question: "Pick a color?" }, { question: "Pick a size?" }] },
        "answered",
        { answers: [["Blue"], []] },
      )),
      tool("call_6", "execute", 2, {
        status: "completed",
        input: { code: "await tools.x()" },
        content: [{ type: "text", text: "TypeError: tools.x is not a function\n    at line 1\n    at line 2\n    at line 3\n    at line 4" }],
        metadata: {
          error: true,
          toolCalls: [
            { tool: "opencode.models", status: "completed", input: { query: "deepseek" } },
            { tool: "opencode.session_rename", status: "error", input: { title: "x" } },
            { tool: "opencode.worktree_list", status: "running" },
          ],
        },
      }),
      tool("call_7", "skill", 2, done({ id: "review" }, "Skill body", { name: "Code Review" })),
      tool("call_8", "custom_tool", 2, done({ target: "prod", force: true, count: 3, nested: { a: 1 } }, "custom output")),
      tool("call_9", "webfetch", 2, failed({ url: "https://example.com/down" }, "Request failed with status 503")),
      tool("call_10", "question", 2, failed({ questions: [{ question: "Ignored?" }] }, "QuestionRejectedError: the user dismissed this question")),
      { type: "text", text: "All states shown." },
    ]),
  ])
  await t.waitFor("All states shown.")
  await t.frame("bottom")
  await t.press("g", { ctrl: true })
  await t.frame("top")
})

scenario("in-flight tool states from history", { height: 40, tui: { session: { grouping: "none" } } }, async (t) => {
  await open(t, "Running history", [
    user(1, "Start several things"),
    {
      ...assistant(2, [
        { type: "reasoning", text: "**Working**\n\nStill going.", time: { created: at(2) } },
        { type: "text", text: "Starting work." },
        { type: "tool", id: "call_1", name: "shell", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_2", name: "write", state: { status: "streaming", input: '{"path":' }, time: { created: at(2) } },
        { type: "tool", id: "call_3", name: "edit", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_4", name: "read", state: { status: "running", input: { path: "a.txt" }, metadata: {} }, time: { created: at(2), ran: at(2) } },
        { type: "tool", id: "call_5", name: "glob", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_6", name: "subagent", state: { status: "running", input: { agent: "general", description: "Dig in", prompt: "x" }, metadata: {} }, time: { created: at(2), ran: at(2) } },
        { type: "tool", id: "call_7", name: "todowrite", state: { status: "running", input: { todos: [] }, metadata: {} }, time: { created: at(2), ran: at(2) } },
        { type: "tool", id: "call_8", name: "question", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_9", name: "apply_patch", state: { status: "streaming", input: '{"patchText": "*** Update File: a.ts' }, time: { created: at(2) } },
        { type: "tool", id: "call_10", name: "skill", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_11", name: "webfetch", state: { status: "streaming", input: "" }, time: { created: at(2) } },
        { type: "tool", id: "call_12", name: "execute", state: { status: "running", input: { code: "1" }, metadata: { toolCalls: [{ tool: "opencode.models", status: "running" }] } }, time: { created: at(2), ran: at(2) } },
      ]),
      finish: undefined,
    },
  ])
  await t.waitFor("Starting work.")
  await t.frame("history")
})

scenario("notices, shells, compactions and errors from history", { height: 90 }, async (t) => {
  const parent = await open(
    t,
    "Notice history",
    [
      user(1, "Look at these @notes.txt and $review", {
        files: [
          { data: Buffer.from("alpha\n").toString("base64"), mime: "text/plain", source: { type: "uri", uri: "file:///project/notes.txt" }, name: "notes.txt" },
          { data: "", mime: "application/x-directory", source: { type: "uri", uri: "file:///project/docs" }, name: "docs" },
          { data: Buffer.from("plain").toString("base64"), mime: "text/plain", source: { type: "inline" } },
        ],
        skills: [{ id: "review", name: "review", text: "Review carefully." }],
      }),
      { id: mid(), type: "agent-switched", agent: "plan", previous: "build", time: { created: at(2) } },
      { id: mid(), type: "agent-switched", agent: "build", time: { created: at(3) } },
      { id: mid(), type: "model-switched", model: { ...MODEL, variant: "low" }, previous: MODEL, time: { created: at(4) } },
      { id: mid(), type: "location-switched", location: { directory: "/somewhere/else" }, subpath: "", time: { created: at(5) } },
      { id: mid(), type: "system", text: "Instructions changed", description: "AGENTS.md updated", time: { created: at(6) } },
      { id: mid(), type: "system", text: "No description", time: { created: at(7) } },
      { id: mid(), type: "skill", skill: "review", name: "review", text: "Skill body", time: { created: at(8) } },
      { id: mid(), type: "synthetic", text: "x", description: "Indexed 3 pages", metadata: { source: "subagent", state: "completed", agent: "general", childID: "ses_00000000000000conformanceZZ" }, time: { created: at(9) } },
      { id: mid(), type: "synthetic", text: "x", description: "Crashed while indexing", metadata: { source: "subagent", state: "error", agent: "explore" }, time: { created: at(10) } },
      { id: mid(), type: "synthetic", text: "x", description: "npm   run\n  dev", metadata: { source: "shell", state: "cancelled" }, time: { created: at(11) } },
      { id: mid(), type: "synthetic", text: "x", description: "Goal reached", metadata: { source: "goal" }, time: { created: at(12) } },
      { id: mid(), type: "synthetic", text: "x", description: "Client note", metadata: { source: "client" }, time: { created: at(13) } },
      { id: mid(), type: "shell", shellID: "sh_00000000000000conformance01", command: "ls", status: "exited", exit: 0, output: { output: "a.txt\n", cursor: 6, size: 6, truncated: false }, time: { created: at(14), completed: at(15) } },
      { id: mid(), type: "shell", shellID: "sh_00000000000000conformance02", command: "false", status: "exited", exit: 1, output: { output: "", cursor: 0, size: 0, truncated: false }, time: { created: at(16), completed: at(17) } },
      { id: mid(), type: "shell", shellID: "sh_00000000000000conformance03", command: "sleep 100", status: "killed", time: { created: at(18), completed: at(19) } },
      { id: mid(), type: "shell", shellID: "sh_00000000000000conformance04", command: "sleep 200", status: "timeout", time: { created: at(20), completed: at(21) } },
      assistant(22, [{ type: "text", text: "Before compaction." }]),
      { id: mid(), type: "compaction", status: "completed", reason: "manual", summary: "## Objective\n- Keep going.", model: MODEL, tokens: tokens(4000, 300), time: { created: at(23) } },
      { id: mid(), type: "compaction", status: "completed", reason: "auto", summary: "Provider summary.", native: { messages: [] }, time: { created: at(24) } },
      { id: mid(), type: "compaction", status: "failed", reason: "auto", error: { type: "provider", message: "context too long" }, time: { created: at(25) } },
      { id: mid(), type: "compaction", status: "failed", reason: "manual", error: { type: "aborted", message: "aborted" }, time: { created: at(26) } },
      assistant(27, [{ type: "text", text: "Partial answer" }], {
        error: { type: "provider", message: "Upstream overloaded", status: 529 },
        finish: "error",
      }),
      assistant(28, [], {
        retry: { attempt: 3, at: at(28) + 1000, error: { type: "provider", message: "Rate limited" } },
        finish: undefined,
        time: { created: at(28), completed: at(28) + 500 },
      }),
      assistant(29, [{ type: "text", text: "Stopped here" }], { error: { type: "aborted", message: "Step interrupted" } }),
      { id: mid(), type: "idle", outcome: "failed", error: { type: "provider", message: "The session failed for good" }, time: { created: at(30) } },
      { id: mid(), type: "idle", outcome: "succeeded", time: { created: at(31) } },
    ],
  )
  void parent
  await t.waitFor("The session failed for good")
  await t.frame("bottom")
  await t.press("g", { ctrl: true })
  await t.frame("top")
})

// Image previews are left off: under the harness's network sandbox the TUI's fetch of `data:` image URIs is
// routed to the fake provider and fails, which is not what the product does.
scenario(
  "images in history",
  { height: 40, tui: { session: { grouping: "none" } } },
  async (t) => {
    await open(t, "Image history", [
      user(1, "What is in this picture?", {
        files: [
          { data: PNG, mime: "image/png", source: { type: "inline" }, name: "pixel.png" },
          { data: PNG, mime: "image/png", source: { type: "inline" }, name: "copy.png" },
        ],
      }),
      assistant(2, [
        tool("call_1", "read", 2, {
          status: "completed",
          input: { path: "a.png" },
          content: [
            { type: "text", text: "Image read." },
            { type: "file", uri: `data:image/png;base64,${PNG}`, mime: "image/png", name: "a.png" },
            { type: "file", uri: `data:image/png;base64,${PNG}`, mime: "image/png", name: "b.png" },
            { type: "file", uri: `data:image/png;base64,${PNG}`, mime: "image/png", name: "c.png" },
            { type: "file", uri: `data:image/png;base64,${PNG}`, mime: "image/png", name: "d.png" },
          ],
        }),
        { type: "text", text: "A single pixel." },
      ]),
    ])
    await t.waitFor("A single pixel.")
    await t.frame("history")
  },
)

scenario(
  "turn token usage diagnostics",
  { height: 40, tui: { debug: { turn_tokens: "verbose" } } },
  async (t) => {
    await open(t, "Token history", [
      user(1, "Count"),
      assistant(2, [tool("call_1", "read", 2, done({ path: "a.txt", limit: 5 }, "a"))], {
        finish: "tool-calls",
        tokens: { input: 1200, output: 40, reasoning: 10, cache: { read: 8000, write: 100 } },
      }),
      assistant(3, [{ type: "text", text: "Counted." }], {
        tokens: { input: 900, output: 30, reasoning: 0, cache: { read: 200, write: 0 } },
      }),
    ])
    await t.waitFor("Counted.")
    await t.frame("history")
  },
)

scenario("a session whose directory is gone", { files: { "gone/keep.txt": "x\n" } }, async (t) => {
  const directory = path.join(t.project, "gone")
  rmSync(directory, { recursive: true })
  await open(
    t,
    "Moved away",
    [user(1, "Work in gone"), assistant(2, [{ type: "text", text: "Worked there." }])],
    { location: { directory } },
    true,
  )
  await t.waitFor("Worked there.")
  await t.frame("missing")
})
