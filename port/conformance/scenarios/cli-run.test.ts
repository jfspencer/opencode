// `opencode run`: one prompt to completion from the command line. Output formats, session selection
// (--continue, --session, --fork), message sources (arguments, stdin, attachments), model/agent selection,
// titles and archiving, permission handling (auto-reject vs --auto), and failures.
import { writeFileSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say, type Turn } from "../lib/openrouter"
import { cliScenario } from "../lib/cli"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
/** Every tool hidden except the ones listed (keeps recorded provider requests small). */
const only = (...rules: ReturnType<typeof rule>[]) => ({
  permissions: [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny"), ...rules],
})

const reasoned = (reasoning: string, text: string): Turn => ({ events: [{ reasoning }, { text }] })

/** The session ID a `run --format json` printed. */
function sessionOf(stdout: string) {
  const line = stdout.split("\n").find((item) => item.includes('"sessionID"'))
  if (!line) throw new Error(`no session in output: ${stdout}`)
  return JSON.parse(line).sessionID as string
}

cliScenario("run prints the reply with the reference config", { turns: [say("Hello from the fake model.")] }, async (ctx) => {
  await ctx.cli(["run", "Say", "hello", "to", "me"])
})

cliScenario(
  "run with tool calls edits files",
  {
    files: { "src/math.ts": "export const add = (a: number, b: number) => a + b\n" },
    config: only(rule("read", "*", "allow"), rule("edit", "*", "allow"), rule("write", "*", "allow")),
    turns: [
      call("read", { path: "src/math.ts" }, "Reading the file first."),
      call("edit", { path: "src/math.ts", oldString: "a + b", newString: "b + a" }),
      call("write", { path: "NOTES.md", content: "# Notes\n\nSwapped the operands.\n" }),
      say("Swapped the operands and wrote notes."),
    ],
  },
  async (ctx) => {
    await ctx.cli(["run", "Swap the operands in src/math.ts"])
  },
)

cliScenario(
  "run with a failing tool call",
  {
    config: only(rule("read", "*", "allow")),
    turns: [call("read", { path: "missing.txt" }), say("The file does not exist.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "Read missing.txt"])
  },
)

cliScenario(
  "run --format json streams events",
  {
    files: { "README.md": "# Demo\n" },
    config: only(rule("read", "*", "allow")),
    turns: [call("read", { path: "README.md" }, "Let me look."), reasoned("The README is short.", "It is a demo project.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "--format", "json", "What is this project?"])
  },
)

cliScenario(
  "run --thinking shows reasoning",
  {
    config: only(),
    turns: [reasoned("Two plus two is four.", "4"), reasoned("Three plus three is six.", "6"), reasoned("Hidden in JSON?", "8")],
  },
  async (ctx) => {
    await ctx.cli(["run", "What is 2+2?"])
    await ctx.cli(["run", "--thinking", "What is 3+3?"])
    await ctx.cli(["run", "--thinking", "--format", "json", "What is 4+4?"])
  },
)

cliScenario(
  "run continues sessions with --continue, --session and --fork",
  {
    config: only(),
    turns: [say("First answer."), say("Second answer."), say("Third answer."), say("Fourth answer."), say("Fifth answer."), say("Sixth answer.")],
  },
  async (ctx) => {
    const first = await ctx.cli(["run", "--format", "json", "First question"])
    const id = sessionOf(first.stdout)
    await ctx.cli(["run", "--continue", "Second question"])
    await ctx.cli(["run", "--session", id, "Third question"])
    await ctx.cli(["run", "-c", "--fork", "Fourth question, in a fork"])
    // A session ID that does not exist yet creates the session with that ID.
    await ctx.cli(["run", "-s", "ses_cliConformanceSession001", "Fifth question"])
    await ctx.cli(["run", "-c", "Sixth question"])
    await ctx.cli(["session", "list", "--format", "json"])
  },
)

cliScenario(
  "run reads the message from stdin",
  { config: only(), turns: [say("Got the piped text."), say("Got both.")] },
  async (ctx) => {
    await ctx.cli(["run"], { stdin: "Summarize this piped text.\nSecond line.\n" })
    await ctx.cli(["run", "Review the context below"], { stdin: "context from a pipe\n" })
  },
)

cliScenario(
  "run with attached files",
  {
    files: { "notes.txt": "alpha\nbeta\n", "data/config.json": '{ "debug": true }\n' },
    config: only(),
    turns: [say("Read the notes."), say("Saw the image."), say("Not reached.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "--file", "notes.txt", "-f", "data/config.json", "What do these files say?"])
    // A 1x1 PNG.
    writeFileSync(
      path.join(ctx.project, "pixel.png"),
      Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64"),
    )
    await ctx.cli(["run", "-f", "pixel.png", "Describe the image"])
    await ctx.cli(["run", "-f", "missing.txt", "Read it"])
    await ctx.cli(["run", "-f", "data", "Read the directory"])
  },
)

cliScenario(
  "run selects the model, variant and agent",
  {
    config: only(),
    turns: [say("Low effort reply."), say("Planning reply."), say("Not reached."), say("Not reached either.")],
  },
  async (ctx) => {
    // Explicit titles: title generation is asynchronous and races the process exiting (and the next run).
    await ctx.cli(["run", "--title", "low", "--model", "openrouter/deepseek/deepseek-v4.1-flash-1017#low", "Quick question"])
    await ctx.cli(["run", "--title", "plan", "--agent", "plan", "Plan the work"])
    await ctx.cli(["run", "--title", "no model", "-m", "openrouter/no-such-model", "Hello"])
    await ctx.cli(["run", "--title", "other provider", "-m", "anthropic/claude-sonnet", "Hello"])
    await ctx.cli(["run", "--title", "no agent", "--agent", "no-such-agent", "Hello"])
    await ctx.cli(["run", "--title", "no variant", "-m", "openrouter/deepseek/deepseek-v4.1-flash-1017#ultra", "Hello"])
    await ctx.cli(["session", "list", "--format", "json"])
  },
)

cliScenario(
  "run sets the title and archives",
  { config: only(), turns: [say("Titled."), say("Archived."), say("Default title."), say("Continued.")] },
  async (ctx) => {
    await ctx.cli(["run", "--title", "My scripted run", "Hello"])
    await ctx.cli(["run", "--archived", "Archived run"])
    await ctx.cli(["run", "--title", "", "An empty title uses the start of this rather long message as the title"])
    await ctx.cli(["session", "list", "--format", "json"])
    // Continuing a session leaves its archived state alone.
    await ctx.cli(["run", "-c", "--archived", "Continue the latest"])
    await ctx.cli(["api", "GET", "/api/session?archived=true"])
  },
)

const shellAsks = only(rule("shell", "*", "ask"), rule("shell", "rm *", "deny"))

cliScenario(
  "run auto-rejects permission requests without --auto",
  {
    files: { "notes.txt": "alpha\n" },
    config: shellAsks,
    turns: [call("shell", { command: "cat notes.txt" }), say("I could not read it.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "Show notes.txt"])
  },
)

cliScenario(
  "run --auto approves permission requests",
  {
    files: { "notes.txt": "alpha\n" },
    config: shellAsks,
    turns: [call("shell", { command: "cat notes.txt" }), call("shell", { command: "touch created.txt" }), say("Read it and created a file.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "--auto", "Show notes.txt and create created.txt"])
  },
)

cliScenario(
  "run --auto does not approve denied actions",
  {
    files: { "notes.txt": "alpha\n" },
    config: shellAsks,
    turns: [call("shell", { command: "rm notes.txt" }), say("Deleting is not allowed.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "--auto", "Delete notes.txt"])
  },
)

cliScenario(
  "run --yolo and --dangerously-skip-permissions are --auto",
  {
    config: shellAsks,
    turns: [call("shell", { command: "echo one" }), say("One."), call("shell", { command: "echo two" }), say("Two.")],
  },
  async (ctx) => {
    await ctx.cli(["run", "--yolo", "Echo one"])
    await ctx.cli(["run", "--dangerously-skip-permissions", "Echo two"])
  },
)

cliScenario(
  "run cancels questions it cannot ask",
  {
    config: only(rule("question", "*", "allow")),
    turns: [
      call("question", {
        questions: [{ question: "Which color?", header: "Color", options: [{ label: "Blue", description: "Calm" }, { label: "Red", description: "Bold" }] }],
      }),
      say("I'll pick blue."),
    ],
  },
  async (ctx) => {
    await ctx.cli(["run", "Style the button"])
  },
)

cliScenario("run argument errors", { config: only() }, async (ctx) => {
  await ctx.cli(["run"])
  await ctx.cli(["run"], { stdin: "   \n" })
  await ctx.cli(["run", "--fork", "Hello"])
  await ctx.cli(["run", "--worktree", "-c", "Hello"])
  await ctx.cli(["run", "--format", "json"])
  await ctx.cli(["run", "--session", "ses_doesNotExist0000000000001", "--fork", "Hello"])
})

cliScenario(
  "run reports model failures",
  {
    config: only(),
    turns: [
      { status: 401, body: { error: { message: "No auth credentials found", code: 401 } } },
      { status: 400, body: { error: { message: "Bad request: context too long", code: 400 } } },
    ],
  },
  async (ctx) => {
    await ctx.cli(["run", "Hello"])
    await ctx.cli(["run", "--format", "json", "Hello again"])
  },
)

cliScenario(
  "run --worktree starts the session in a new git worktree",
  { git: true, files: { "README.md": "# Repo\n" }, config: only(), turns: [say("Working in the worktree.")] },
  async (ctx) => {
    // The name is an optional flag value: `--worktree=<name>` (a separate word would be part of the message).
    await ctx.cli(["run", "--worktree=feature-x", "--format", "json", "Hello"])
    await ctx.cli(["session", "list", "--format", "json"])
  },
)

const released = Promise.withResolvers<void>()

cliScenario(
  "run interrupted by SIGTERM",
  {
    config: only(),
    turns: [{ events: [{ text: "Working on it" }, { until: released.promise }] }, say("Resumed.")],
  },
  async (ctx) => {
    const run = ctx.start(["run", "Do something slow"], { stdin: "ignore" })
    const deadline = Date.now() + 30_000
    while (!ctx.provider.requests.some((request) => request.kind === "chat")) {
      if (Date.now() > deadline) throw new Error("the run never reached the model")
      await Bun.sleep(25)
    }
    // Interrupt once the run has printed its header.
    await run.line((line) => line.includes("> build"), "stderr")
    await run.stop("SIGTERM")
    released.resolve()
    await ctx.cli(["run", "-c", "Continue"])
  },
)
