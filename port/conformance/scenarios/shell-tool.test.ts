// The shell tool's execution behavior: output capture, exit status, timeouts, truncation, working directories,
// environment, background commands and their notifications, and commands that end before completing.
// Every tool except the shell is denied (and so hidden) to keep the recorded provider requests small.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type Turn } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"
import { existsSync } from "node:fs"
import path from "node:path"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
const only = [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")]
// A one-line system prompt keeps the recorded provider requests small; the default prompt is covered elsewhere.
const agents = { build: { system: "Conformance agent." } }
const allowShell = { agents, permissions: [...only, rule("shell", "*", "allow")] }
/** The sandbox root that contains the project directory (recorded as <root>). */
const root = process.env.CONFORMANCE_ROOT!

const files = {
  "README.md": "# Demo\n",
  "src/a.txt": "alpha\n",
  "src/b.txt": "beta\n",
  "lines.txt": Array.from({ length: 30 }, (_, index) => `line ${index + 1}`).join("\n") + "\n",
}

type Decide = (request: any, ctx: Ctx) => Record<string, unknown>
/** Prompt, answering every permission request with `decide`, until the session is idle. */
async function drive(ctx: Ctx, id: string, text: string, decide: Decide = () => ({ decision: "once" })) {
  let from = ctx.events.length
  await ctx.http("POST", `/api/session/${id}/prompt`, { text })
  let state = await ctx.settle(id, from)
  while (state === "permission.asked") {
    const asked = ctx.events.filter((event: any) => event?.type === "permission.asked" && event.data.sessionID === id).at(-1) as any
    from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/permission/${asked.data.id}/reply`, decide(asked.data, ctx))
    state = await ctx.settle(id, from)
  }
}

/** One scenario: the model runs the given shell calls one per step, then says it is done. */
function shell(name: string, inputs: Record<string, unknown>[], options: { config?: Record<string, unknown>; files?: Record<string, string>; decide?: Decide } = {}) {
  const turns: Turn[] = [...inputs.map((input) => call("shell", input)), say("Done.")]
  scenario(name, { files: options.files ?? files, config: options.config ?? allowShell, turns }, async (ctx) => {
    const id = await ctx.session({ title: name })
    await drive(ctx, id, name, options.decide)
  })
}

// Output combines stdout and stderr, two pipes read concurrently: a command that writes to both interleaves them
// in whatever order they are read, so each command here writes to one.
shell("output, stderr and exit status", [
  { command: "cat src/a.txt src/b.txt" },
  { command: "echo err >&2; echo again >&2" },
  { command: "true" },
  { command: "grep -c missing README.md" },
  { command: "echo partial; exit 3" },
  { command: "ls does-not-exist" },
  { command: "printf 'no trailing newline'" },
  { command: "kill -TERM $$" },
])

shell("the environment of shell commands", [
  { command: 'echo "$AGENT $OPENCODE $OPENCODE_TERMINAL $TERM"' },
  { command: 'test "$OPENCODE_SESSION_ID" = "$(printf %s "$OPENCODE_SESSION_ID" | grep "^ses_")" && echo session id set' },
  { command: "pwd" },
])

// The session environment replaces the process environment of its shell commands. Agent markers are added on top;
// AI_AGENT keeps a value the environment already has and defaults to "opencode".
scenario(
  "a session environment replaces the environment of shell commands",
  {
    files,
    config: allowShell,
    turns: [
      call("shell", { command: 'echo "FOO=$FOO AI_AGENT=$AI_AGENT AGENT=$AGENT HOME=$HOME"' }),
      call("shell", { command: 'echo "AI_AGENT=$AI_AGENT"' }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "environment" })
    await ctx.http("PUT", `/api/session/${id}/environment`, { variables: { FOO: "bar" } })
    await drive(ctx, id, "Show the environment")
    await ctx.http("PUT", `/api/session/${id}/environment`, { variables: { AI_AGENT: "custom-agent" } })
    ctx.provider.push(call("shell", { command: 'echo "AI_AGENT=$AI_AGENT"' }), say("Done."))
    await drive(ctx, id, "Show it again")
    await ctx.http("PUT", "/api/session/ses_conformance_missing/environment", { variables: {} })
  },
)

shell("working directories", [
  { command: "pwd && ls", workdir: "src" },
  { command: "cat a.txt", workdir: "./src/" },
  { command: "pwd", workdir: "missing" },
  { command: "pwd", workdir: "README.md" },
  { command: "cd src && pwd && cat a.txt" },
  { command: "cd src; cd ..; pwd" },
  { command: "cd missing" },
])

shell(
  "working directories outside the project ask for external directory access",
  [
    { command: "pwd", workdir: ".." },
    { command: "cd .. && pwd" },
    { command: "cd / && pwd" },
    { command: "cd ../conformance-missing-directory" },
    { command: "ls", workdir: "/" },
    { command: "cd src && cd .. && pwd" },
  ],
  {
    config: { agents, permissions: [...only, rule("shell", "*", "allow"), rule("external_directory", "*", "ask")] },
    // Approve the sandbox root (the project's parent) once; refuse everything else.
    decide: (request) =>
      request.resources.length === 1 && request.resources[0] === `${root}/*`
        ? { decision: "once" }
        : { decision: "reject", message: "Stay inside the project." },
  },
)

shell(
  "external directory rules from config",
  [{ command: "cd .. && pwd" }, { command: "cd / && pwd" }, { command: "pwd", workdir: root }],
  {
    config: {
      agents,
      permissions: [
        ...only,
        rule("shell", "*", "allow"),
        rule("external_directory", "*", "deny"),
        rule("external_directory", `${root}/*`, "allow"),
      ],
    },
  },
)

shell("timeouts", [
  { command: "echo started; sleep 5; echo finished", timeout: 300 },
  { command: "echo quick", timeout: 10_000 },
  { command: "echo no limit", timeout: 0 },
])

shell(
  "output truncated by line count",
  [{ command: "cat lines.txt" }, { command: "head -n 5 lines.txt" }, { command: "seq 1 12 >&2" }],
  { config: { ...allowShell, tool_output: { max_lines: 8 } } },
)

shell(
  "output truncated by size",
  [{ command: "cat lines.txt" }, { command: "printf 'x%.0s' $(seq 1 100)" }, { command: "echo short" }],
  { config: { ...allowShell, tool_output: { max_bytes: 64 } } },
)

shell("large output uses the default line limit", [{ command: "seq 1 2001" }])

shell("commands the scanner cannot analyze are refused", [
  { command: "echo 'unterminated" },
  { command: "eval \"$(printf 'echo %s' hi)\"" },
  { command: "echo $(( $(echo 1) + x[$(echo 2)] ))" },
  { command: "cat <<EOF\nno terminator" },
])

shell("invalid shell inputs", [{ command: "echo hi", timeout: -1 }, { command: "echo hi", background: "yes" }, { workdir: "src" }, { command: 42 }])

shell(
  "commands without permission resources run without asking",
  [{ command: "X=1" }, { command: "export Y=2; unset Y" }, { command: "cd src" }, { command: "# just a comment" }, { command: "" }],
  { config: { agents, permissions: [...only, rule("shell", "*", "ask")] }, decide: () => ({ decision: "reject", message: "No." }) },
)

// Commands that keep running wait for a file the scenario writes, so every step happens in a fixed order.
const waitFor = (file: string, then = `cat ${file}`) => `until [ -f ${file} ]; do sleep 0.05; done; ${then}`
const event = (ctx: Ctx, type: string, id: string, from = 0) =>
  ctx.until((item: any) => item?.type === type && item.data?.sessionID === id, from)
/** Wait until a background shell's completion notice has been handled: its message arrives, then the turn ends. */
async function notified(ctx: Ctx, id: string, from: number) {
  const enqueued = await ctx.until(
    (item: any) =>
      item?.type === "session.inbox.enqueued" && item.data?.sessionID === id && item.data.item?.payload?.metadata?.source === "shell",
    from,
  )
  await event(ctx, "session.execution.succeeded", id, ctx.events.indexOf(enqueued))
}
const shellID = async (ctx: Ctx, id: string, from = 0) => (await event(ctx, "session.tool.progress", id, from)).data.metadata.shellID as string

scenario(
  "background commands notify the session when they finish",
  {
    files,
    config: allowShell,
    turns: [
      call("shell", { command: waitFor("go-1"), background: true }),
      call("shell", { command: waitFor("go-2", "echo failing; exit 4"), background: true }),
      say("Both started."),
      say("The first one finished."),
      say("The second one failed."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "background" })
    await drive(ctx, id, "Run both in the background")
    await ctx.http("GET", "/api/experimental/job")
    let from = ctx.events.length
    ctx.write("go-1", "first result\n")
    await notified(ctx, id, from)
    from = ctx.events.length
    ctx.write("go-2", "")
    await notified(ctx, id, from)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

// Backgrounding returns the blocked shell call at once and adds a steering message for the next step. A second
// call in the same step (a fetch the scenario holds open) keeps that step from ending before the message is added,
// so the next request always carries both. Events are not recorded: the order of the tool result and the message
// is not defined. Waits use the filesystem and the fake provider instead.
const fetchGate = Promise.withResolvers<void>()
scenario(
  "move a running foreground command to the background",
  {
    files,
    events: false,
    config: { agents, permissions: [...only, rule("shell", "*", "allow"), rule("webfetch", "*", "allow")] },
    responders: [
      (request) =>
        request.url === "https://hold.example/" ? { events: [{ until: fetchGate.promise }, { text: "held" }] } : undefined,
    ],
    turns: [
      {
        events: [
          { tool: "shell", input: { command: `touch started; ${waitFor("go")}` } },
          { tool: "webfetch", input: { url: "https://hold.example/", format: "text" } },
        ],
      },
      say("It is running in the background."),
      say("It finished."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "to background" })
    const until = async (check: () => boolean) => {
      while (!check()) await new Promise((resolve) => setTimeout(resolve, 10))
    }
    const chats = () => ctx.provider.requests.filter((request) => request.kind === "chat").length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Run it" })
    await until(() => ctx.provider.requests.some((request) => request.url === "https://hold.example/"))
    await until(() => existsSync(path.join(ctx.project, "started")))
    // The call registers as blocking just after the process starts; give it ample time before backgrounding.
    await new Promise((resolve) => setTimeout(resolve, 500))
    await ctx.http("POST", `/api/session/${id}/background`)
    fetchGate.resolve()
    await until(() => chats() >= 2)
    await ctx.wait(id)
    ctx.write("go", "done\n")
    await until(() => chats() >= 3)
    await ctx.wait(id)
    await ctx.http("POST", `/api/session/${id}/background`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "interrupt a session while a command runs",
  {
    files,
    config: allowShell,
    turns: [call("shell", { command: waitFor("never") }), say("Not reached.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "interrupt" })
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Run it" })
    await shellID(ctx, id, from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.settle(id, from)
    await ctx.http("POST", `/api/session/${id}/interrupt`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "remove the shell of a running command",
  {
    files,
    config: allowShell,
    turns: [
      call("shell", { command: waitFor("never") }),
      call("shell", { command: waitFor("never"), background: true }),
      say("The background one started."),
      say("The background one was removed."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "remove" })
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Run them" })
    const foreground = await shellID(ctx, id, from)
    await ctx.http("DELETE", `/api/shell/${foreground}`)
    from = ctx.events.length
    const background = await shellID(ctx, id, from)
    await ctx.settle(id, from)
    from = ctx.events.length
    await ctx.http("DELETE", `/api/shell/${background}`)
    await notified(ctx, id, from)
  },
)
