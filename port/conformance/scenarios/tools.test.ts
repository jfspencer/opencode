// Each scenario scripts the model to call a tool, then records what the tool did (files, events), what was
// sent back to the model (the next provider request), and the session's messages.
import { call, say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n\nexport const twice = (n: number) => add(n, n)\n",
  "src/strings.ts": "export const greet = (name: string) => `hello ${name}`\nexport const shout = (s: string) => s.toUpperCase()\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
}

async function turn(ctx: Parameters<Parameters<typeof scenario>[2]>[0], text: string) {
  const id = await ctx.session()
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

scenario("read a file", { files, turns: [call("read", { path: "src/math.ts" }), say("It adds numbers.")] }, (ctx) =>
  turn(ctx, "What does src/math.ts do?").then(() => undefined),
)

scenario(
  "read a file with offset and limit",
  { files, turns: [call("read", { path: "notes/todo.txt", offset: 2, limit: 1 }), say("Line two is two.")] },
  (ctx) => turn(ctx, "Read line 2 of notes/todo.txt").then(() => undefined),
)

scenario(
  "read a missing file",
  { files, turns: [call("read", { path: "src/missing.ts" }), say("That file does not exist.")] },
  (ctx) => turn(ctx, "Read src/missing.ts").then(() => undefined),
)

scenario("read a directory", { files, turns: [call("read", { path: "src" }), say("Two files.")] }, (ctx) =>
  turn(ctx, "List src").then(() => undefined),
)

scenario(
  "write a new file",
  { files, turns: [call("write", { path: "src/new.ts", content: "export const x = 1\n" }), say("Created.")] },
  (ctx) => turn(ctx, "Create src/new.ts").then(() => undefined),
)

scenario(
  "overwrite an existing file",
  { files, turns: [call("write", { path: "README.md", content: "# Replaced\n" }), say("Replaced.")] },
  (ctx) => turn(ctx, "Replace the README").then(() => undefined),
)

scenario(
  "edit a file with a unique match",
  {
    files,
    turns: [
      call("read", { path: "src/math.ts" }),
      call("edit", { path: "src/math.ts", oldString: "return a + b", newString: "return a + b + 0" }),
      say("Edited."),
    ],
  },
  (ctx) => turn(ctx, "Edit add").then(() => undefined),
)

scenario(
  "edit with a string that does not occur",
  {
    files,
    turns: [
      call("read", { path: "src/math.ts" }),
      call("edit", { path: "src/math.ts", oldString: "return a - b", newString: "return b" }),
      say("Could not edit."),
    ],
  },
  (ctx) => turn(ctx, "Edit subtraction").then(() => undefined),
)

scenario(
  "edit with an ambiguous match, then replaceAll",
  {
    files: { ...files, "dup.txt": "x\nx\nx\n" },
    turns: [
      call("read", { path: "dup.txt" }),
      call("edit", { path: "dup.txt", oldString: "x", newString: "y" }),
      call("edit", { path: "dup.txt", oldString: "x", newString: "y", replaceAll: true }),
      say("Done."),
    ],
  },
  (ctx) => turn(ctx, "Replace x with y").then(() => undefined),
)

scenario(
  "edit without reading first",
  {
    files,
    turns: [call("edit", { path: "src/strings.ts", oldString: "hello", newString: "hi" }), say("Ok.")],
  },
  (ctx) => turn(ctx, "Change hello to hi").then(() => undefined),
)

// The order of multi-file glob/grep results follows ripgrep's parallel walk and is not deterministic upstream,
// so these pin single-file matches; the order of several results is unspecified by the standard.
scenario("glob for files", { files, turns: [call("glob", { pattern: "**/math.ts" }), say("One TS file.")] }, (ctx) =>
  turn(ctx, "Find math.ts").then(() => undefined),
)

scenario(
  "grep for a pattern",
  { files, turns: [call("grep", { pattern: "export const (greet|shout)", include: "*.ts" }), say("Found exports.")] },
  (ctx) => turn(ctx, "Find exported consts").then(() => undefined),
)

scenario(
  "grep with no matches",
  { files, turns: [call("grep", { pattern: "nothing-matches-this" }), say("No matches.")] },
  (ctx) => turn(ctx, "Search for nothing").then(() => undefined),
)

scenario(
  "shell command",
  { files, turns: [call("shell", { command: "ls src" }), say("Listed.")] },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "List src with the shell" })
    const state = await ctx.settle(id, from)
    if (state === "permission.asked") {
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      const request = pending.body.data[0]
      await ctx.http("POST", `/api/session/${id}/permission/${request.id}/reply`, { decision: "once" })
      await ctx.settle(id)
    }
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "invalid tool input",
  { files, turns: [call("read", { filePath: "README.md" }), say("Retried.")] },
  (ctx) => turn(ctx, "Read something").then(() => undefined),
)

scenario("unknown tool", { files, turns: [call("does_not_exist", {}), say("No such tool.")] }, (ctx) =>
  turn(ctx, "Use a missing tool").then(() => undefined),
)

scenario(
  "malformed tool arguments",
  { files, turns: [{ events: [{ tool: "read", rawInput: '{"filePath": "README.md"' }], finish: "tool_calls" }, say("Oops.")] },
  (ctx) => turn(ctx, "Read the README").then(() => undefined),
)

scenario(
  "two tool calls in one step",
  {
    files,
    turns: [
      { events: [{ tool: "read", input: { path: "README.md" } }, { tool: "glob", input: { pattern: "src/math*" } }] },
      say("Read and listed."),
    ],
  },
  (ctx) => turn(ctx, "Read README and list src").then(() => undefined),
)
