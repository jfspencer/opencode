// Tool output bounding (core/src/tool-output.ts, config/plugin/tool-output.ts): a local tool result beyond the
// configured `tool_output` limits (else 2,000 lines and 50 KiB) is cut at a line boundary, its full text saved under
// `<data>/opencode/tool-output/tool_<id>`, and a marker names the file. Results whose tools bound their own output
// (read, glob, grep, shell) say so in their metadata and pass unchanged. Saved outputs older than seven days are swept
// when the server starts (and hourly after: TS has no clock to inject, so only the startup sweep is observable).
import { existsSync, mkdirSync, readdirSync, utimesSync, writeFileSync } from "node:fs"
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const lines = (count: number, prefix = "line") =>
  `return Array.from({ length: ${count} }, (_, i) => "${prefix} " + (i + 1)).join("\\n")`

async function run(ctx: Ctx, text: string) {
  const id = await ctx.session({ title: "output" })
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}/message`)
  return id
}

scenario(
  "configured line limit bounds a tool result",
  { config: { tool_output: { max_lines: 3 } }, turns: [call("execute", { code: lines(10) }), say("Bounded.")] },
  (ctx) => run(ctx, "Print ten lines").then(() => undefined),
)

scenario(
  "configured byte limit bounds a tool result",
  { config: { tool_output: { max_bytes: 40 } }, turns: [call("execute", { code: lines(10, "a longer line") }), say("Bounded.")] },
  (ctx) => run(ctx, "Print ten long lines").then(() => undefined),
)

scenario(
  "a first line beyond the byte limit shows no lines",
  { config: { tool_output: { max_bytes: 8 } }, turns: [call("execute", { code: `return "x".repeat(20) + "\\nsecond"` }), say("Nothing shown.")] },
  (ctx) => run(ctx, "Print a long line").then(() => undefined),
)

scenario(
  "output within the configured limits is untouched",
  { config: { tool_output: { max_lines: 10, max_bytes: 1000 } }, turns: [call("execute", { code: lines(10) }), say("Whole.")] },
  (ctx) => run(ctx, "Print ten lines").then(() => undefined),
)

scenario(
  "a trailing newline does not count as a line",
  { config: { tool_output: { max_lines: 2 } }, turns: [call("execute", { code: `return "one\\ntwo\\n"` }), say("Two lines.")] },
  (ctx) => run(ctx, "Print two lines").then(() => undefined),
)

scenario(
  "the default limits bound a long result",
  { turns: [call("execute", { code: lines(2_100) }), say("Bounded.")] },
  (ctx) => run(ctx, "Print many lines").then(() => undefined),
)

scenario(
  "tools that bound their own output keep their result",
  {
    config: { tool_output: { max_lines: 2 } },
    files: { "notes.txt": "one\ntwo\nthree\nfour\nfive\n" },
    turns: [call("read", { path: "notes.txt" }), say("Read it.")],
  },
  (ctx) => run(ctx, "Read the notes").then(() => undefined),
)

scenario(
  "a fetched page beyond the limits is saved",
  {
    config: { tool_output: { max_lines: 4 } },
    responders: [
      (request) =>
        request.url === "https://example.com/list.txt"
          ? { raw: Array.from({ length: 12 }, (_, i) => `item ${i + 1}`).join("\n"), headers: { "content-type": "text/plain" } }
          : undefined,
    ],
    turns: [call("webfetch", { url: "https://example.com/list.txt", format: "text" }), say("Fetched.")],
  },
  (ctx) => run(ctx, "Fetch the list").then(() => undefined),
)

// The sweep runs at startup, so the files are planted before a restart. A saved output is `tool_` and 12 hex digits;
// anything else, and anything a plain remove cannot delete (a directory), stays.
scenario(
  "saved outputs older than seven days are removed at startup",
  { processOnly: "the sweep runs when a server starts: the scenario restarts the server process", timeout: 60_000 },
  async (ctx) => {
    const directory = path.join(process.env.XDG_DATA_HOME!, "opencode", "tool-output")
    mkdirSync(directory, { recursive: true })
    const old = new Date(Date.now() - 8 * 24 * 60 * 60 * 1000)
    const recent = new Date(Date.now() - 6 * 24 * 60 * 60 * 1000)
    const plant = (name: string, time: Date, dir = false) => {
      const file = path.join(directory, name)
      if (dir) mkdirSync(file, { recursive: true })
      else writeFileSync(file, name)
      utimesSync(file, time, time)
      return name
    }
    const planted = [
      plant("tool_0000000000aaZZZZZZZZZZZZZZ", old),
      plant("tool_0000000000abZZZZZZZZZZZZZZ.png", old),
      plant("tool_0000000000acZZZZZZZZZZZZZZ", recent),
      plant("tool_0000000000ad", old),
      plant("tool_00000000ae", old),
      plant("tool_0000000000AFZZZZZZZZZZZZZZ", old),
      plant("other_0000000000ag", old),
      plant("tool_0000000000ahZZZZZZZZZZZZZZ", old, true),
    ]
    await ctx.restart()
    await ctx.http("GET", "/api/agent")
    // The sweep runs beside the boot: wait until the oldest saved output is gone (bounded).
    const deadline = Date.now() + 10_000
    while (existsSync(path.join(directory, planted[0])) && Date.now() < deadline) await Bun.sleep(20)
    const present = new Set(readdirSync(directory))
    ctx.note(
      "kept",
      planted.map((name) => ({ name, kept: present.has(name) })),
    )
  },
)
