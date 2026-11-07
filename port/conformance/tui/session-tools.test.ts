// Session view: how each tool call renders in the transcript once the turn has finished.
import { call, say, type Turn } from "../lib/openrouter"
import { tuiScenario, type TuiCtx, type TuiOptions } from "../lib/tui"

// Title generation runs concurrently with the first step, so its events would interleave with the step's at
// random. Hold the first step until the session is renamed: the recorded order is then fixed.
function scenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  const titled = Promise.withResolvers<void>()
  const [first, ...rest] = options.turns ?? []
  const turns: Turn[] | undefined =
    first && "events" in first ? [{ ...first, events: [{ until: titled.promise }, ...first.events] }, ...rest] : options.turns
  tuiScenario(name, { ...options, turns }, async (t) => {
    const started = Date.now()
    t.until((event) => event?.type === "session.renamed", 0, 60_000).then(titled.resolve, titled.resolve)
    await body(t)
    // The TUI polls /api/experimental/migration/v1 one second after it starts; let that happen in every
    // scenario so the recorded set of endpoints does not depend on how long the scenario took.
    await new Promise((resolve) => setTimeout(resolve, Math.max(0, 2_000 - (Date.now() - started))))
  })
}

/** Submit a prompt and wait until the turn is over and the TUI has acknowledged it, then for the text. */
async function ask(t: TuiCtx, prompt: string, text: string | RegExp) {
  const from = t.events.length
  await t.type(prompt)
  await t.enter()
  await t.until((event) => event?.type === "session.viewed", from, 60_000)
  await t.waitFor(text)
}

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n\nexport const twice = (n: number) => add(n, n)\n",
  "src/strings.ts": "export const greet = (name: string) => `hello ${name}`\nexport const shout = (s: string) => s.toUpperCase()\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
}

const exploration: Turn[] = [
  call("read", { path: "src/math.ts" }),
  call("read", { path: "notes/todo.txt", offset: 2, limit: 1 }),
  // Glob and grep list several files in no fixed order, so each pattern matches at most one file.
  call("glob", { pattern: "**/m*.ts" }),
  call("glob", { pattern: "*.md", path: "notes" }),
  call("grep", { pattern: "export const", include: "math.ts" }),
  call("grep", { pattern: "shout", path: "src" }),
  say("Explored the project."),
]

scenario("read, glob and grep are grouped", { files, turns: exploration }, async (t) => {
  await ask(t, "Explore the project", "Explored the project.")
  await t.frame("explored")
})

scenario(
  "read, glob and grep expanded at high verbosity",
  { files, height: 40, turns: exploration, tui: { session: { verbosity: "high" } } },
  async (t) => {
    await ask(t, "Explore the project", "Explored the project.")
    await t.frame("explored")
  },
)

scenario(
  "read, glob and grep without grouping",
  { files, height: 40, turns: exploration, tui: { session: { grouping: "none" } } },
  async (t) => {
    await ask(t, "Explore the project", "Explored the project.")
    await t.frame("explored")
  },
)

scenario(
  "tools summarized at low verbosity",
  {
    files,
    turns: [
      call("read", { path: "src/math.ts" }),
      call("shell", { command: "sleep 1; echo hi" }),
      call("write", { path: "out.txt", content: "x\n" }),
      say("Did a few things."),
    ],
    tui: { session: { verbosity: "low" } },
  },
  async (t) => {
    await ask(t, "Do a few things", "Did a few things.")
    await t.frame("summarized")
  },
)

scenario(
  "write a new file",
  {
    files,
    turns: [
      call("write", { path: "src/new.ts", content: "export const x = 1\n\nexport function y() {\n  return x + 1\n}\n" }),
      say("Created src/new.ts."),
    ],
  },
  async (t) => {
    await ask(t, "Create src/new.ts", "Created src/new.ts.")
    await t.frame("written")
  },
)

scenario(
  "overwrite a file in a nested directory",
  {
    files,
    turns: [call("write", { path: "notes/todo.txt", content: "four\nfive\n" }), say("Replaced the todo list.")],
  },
  async (t) => {
    await ask(t, "Replace the todo list", "Replaced the todo list.")
    await t.frame("written")
  },
)

const swap = [
  call("edit", { path: "src/math.ts", oldString: "return a + b", newString: "return b + a // swapped" }),
  say("Swapped the operands."),
]

scenario("edit a file shows a unified diff", { files, turns: swap }, async (t) => {
  await ask(t, "Swap the operands", "Swapped the operands.")
  await t.frame("edited")
})

scenario(
  "edit a file shows a split diff on a wide terminal",
  { files, width: 160, height: 30, turns: swap, tui: { session: { sidebar: "hide" } } },
  async (t) => {
    await ask(t, "Swap the operands", "Swapped the operands.")
    await t.frame("edited")
  },
)

scenario(
  "edit with a forced split view on a narrow terminal",
  { files, width: 80, height: 30, turns: swap, tui: { diffs: { view: "split" } } },
  async (t) => {
    await ask(t, "Swap the operands", "Swapped the operands.")
    await t.frame("edited")
  },
)

scenario(
  "edit replacing every occurrence with unwrapped long lines",
  {
    files: {
      "src/long.ts": `export const message = "${"word ".repeat(30).trim()}"\nexport const other = "word"\n`,
    },
    width: 80,
    turns: [
      call("edit", { path: "src/long.ts", oldString: "word", newString: "term", replaceAll: true }),
      say("Renamed every word."),
    ],
    tui: { diffs: { wrap: "none" } },
  },
  async (t) => {
    await ask(t, "Rename word to term", "Renamed every word.")
    await t.frame("edited")
  },
)

scenario(
  "shell command with output",
  { files, turns: [call("shell", { command: "sleep 1; ls src && echo done" }), say("Listed.")] },
  async (t) => {
    await ask(t, "List src", "Listed.")
    await t.frame("listed")
  },
)

scenario(
  "shell command that fails",
  { files, turns: [call("shell", { command: "sleep 1; cat missing.txt; exit 3" }), say("It failed.")] },
  async (t) => {
    await ask(t, "Cat a missing file", "It failed.")
    await t.frame("failed")
  },
)

scenario(
  "shell command with long output is collapsed",
  { files, height: 40, turns: [call("shell", { command: "sleep 1; seq 1 40" }), say("Counted.")] },
  async (t) => {
    await ask(t, "Count to 40", "Counted.")
    await t.frame("counted")
  },
)

scenario(
  "shell command with long lines of output",
  {
    files,
    width: 60,
    height: 40,
    tui: { session: { tps: false } },
    turns: [call("shell", { command: "sleep 1; for i in 1 2 3 4 5 6; do printf 'x%.0s' $(seq 1 150); echo; done" }), say("Printed.")],
  },
  async (t) => {
    await ask(t, "Print long lines", "Printed.")
    await t.frame("printed")
  },
)

scenario(
  "shell command in a working directory",
  { files, turns: [call("shell", { command: "sleep 1; pwd | wc -c && ls", workdir: "src" }), say("Ran in src.")] },
  async (t) => {
    await ask(t, "List src from inside it", "Ran in src.")
    await t.frame("ran")
  },
)

scenario(
  "shell command without output",
  { files, turns: [call("shell", { command: "sleep 1; true" }), say("Nothing printed.")] },
  async (t) => {
    await ask(t, "Run true", "Nothing printed.")
    await t.frame("ran")
  },
)

scenario(
  "shell command that times out",
  { files, timeout: 60_000, turns: [call("shell", { command: "sleep 5", timeout: 1500 }), say("It timed out.")] },
  async (t) => {
    await ask(t, "Sleep a while", "It timed out.")
    await t.frame("timed-out")
  },
)

scenario(
  "tool errors",
  {
    files,
    height: 40,
    turns: [
      call("read", { path: "src/missing.ts" }),
      call("edit", { path: "src/math.ts", oldString: "not present", newString: "x" }),
      call("write", { path: "/dev/null/impossible.txt", content: "x" }),
      call("read", { filePath: "README.md" }),
      call("does_not_exist", { value: 1, flag: true, label: "some   spaced   text" }),
      { events: [{ tool: "glob", rawInput: '{"pattern": "**/*' }], finish: "tool_calls" },
      say("Several things went wrong."),
    ],
    tui: { session: { grouping: "none" } },
  },
  async (t) => {
    await ask(t, "Try some things", "Several things went wrong.")
    await t.frame("errors")
  },
)

scenario(
  "webfetch answered by a responder",
  {
    files,
    responders: [
      (request) =>
        request.url.startsWith("https://example.com/")
          ? { raw: "<html><body><h1>Example</h1><p>Hello page.</p></body></html>", headers: { "content-type": "text/html" } }
          : undefined,
    ],
    turns: [
      call("webfetch", { url: "https://example.com/page", format: "markdown" }),
      call("webfetch", { url: "https://example.com/missing" }),
      say("Fetched the page."),
    ],
    tui: { session: { grouping: "none" } },
  },
  async (t) => {
    await ask(t, "Fetch the page", "Fetched the page.")
    await t.frame("fetched")
  },
)

scenario(
  "skill tool",
  { files, turns: [call("skill", { id: "opencode" }), call("skill", { id: "no-such-skill" }), say("Loaded the skill.")] },
  async (t) => {
    await ask(t, "Load the opencode skill", "Loaded the skill.")
    await t.frame("loaded")
  },
)

scenario(
  "execute with nested tool calls",
  {
    files,
    turns: [
      call("execute", {
        code: "const r = await tools.opencode.session_rename({ title: 'Renamed by code' }); return r.title",
      }),
      call("execute", { code: "const m = await tools.opencode.models({ query: 'deepseek', limit: 1 }); return m.total" }),
      call("execute", { code: "throw new Error('boom from code')" }),
      say("Ran code."),
    ],
  },
  async (t) => {
    await ask(t, "Run some code", "Ran code.")
    await t.frame("executed")
  },
)
