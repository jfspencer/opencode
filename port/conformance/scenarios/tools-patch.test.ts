// The patch and todowrite tools, on a GPT model. A `gpt-` model gets `patch` in place of `edit` and `write`
// (tool/plugin/patch.ts) and the GPT system prompt (plugin/optimize.ts); the reference model is not a GPT one, so
// these scenarios define one in config. todowrite is behind `experimental.todo`. The project's files after each
// turn are part of the recording.
import { rmSync } from "node:fs"
import path from "node:path"
import { baseConfig } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"

const GPT = "openai/gpt-5.1"
const gpt = {
  model: `openrouter/${GPT}`,
  agents: { build: { model: `openrouter/${GPT}` } },
  providers: {
    openrouter: {
      models: {
        [GPT]: {
          name: "GPT 5.1",
          package: "@opencode/ai/providers/openrouter",
          capabilities: { tools: true, input: ["text"], output: ["text"], temperature: true },
          limit: { context: 400000, output: 128000 },
        },
      },
    },
  },
  experimental: { todo: true },
}
const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n    return a + b\n}\n\nexport const twice = (n: number) => add(n, n)\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
  "bom.txt": "﻿with bom\nline\n",
}
const patch = (...body: string[]) => ["*** Begin Patch", ...body, "*** End Patch"].join("\n")

async function turn(ctx: Ctx, text: string) {
  const id = await ctx.session()
  await ctx.prompt(id, text)
  await ctx.http("GET", `/api/session/${id}/message`)
}

const cases: Record<string, string> = {
  "add update delete and move": patch(
    "*** Add File: src/new.ts",
    "+export const x = 1",
    "*** Update File: src/math.ts",
    "@@ export function add(a: number, b: number) {",
    "-    return a + b",
    "+    return a + b + 0",
    "*** Delete File: README.md",
    "*** Update File: notes/todo.txt",
    "*** Move to: notes/done.txt",
    " one",
    "-two",
    "+TWO",
  ),
  "a bom file": patch("*** Update File: bom.txt", " with bom", "-line", "+LINE"),
  "lines that are not there": patch("*** Update File: src/math.ts", "-nothing like this", "+x"),
  "a missing file": patch("*** Update File: src/missing.ts", "-a", "+b"),
  "delete then update": patch("*** Delete File: README.md", "*** Update File: README.md", "-# Demo", "+# X"),
  "bad envelope": "*** Update File: a.txt\n-a\n+b",
  "empty patch": patch(),
}

for (const [name, patchText] of Object.entries(cases))
  scenario(`patch: ${name}`, { files, config: gpt, turns: [call("patch", { patchText }), say("Done.")] }, (ctx) =>
    turn(ctx, "Patch it"),
  )

scenario("patch: empty text", { files, config: gpt, turns: [call("patch", { patchText: "" }), say("Done.")] }, (ctx) =>
  turn(ctx, "Patch it"),
)

// `external_directory` is allowed by default (FINDINGS), so the file is written next to the project, in the
// sandbox root that every scenario of the run shares: record it, then remove it.
scenario(
  "patch: outside the project",
  { files, config: gpt, turns: [call("patch", { patchText: patch("*** Add File: ../outside.txt", "+x") }), say("Done.")] },
  async (ctx) => {
    await turn(ctx, "Patch it")
    const outside = path.join(path.dirname(ctx.project), "outside.txt")
    let content: string | null = null
    try {
      content = await Bun.file(outside).text()
    } catch {}
    ctx.note("outside.txt", content)
    rmSync(outside, { force: true })
  },
)

scenario(
  "todowrite: update and clear",
  {
    files,
    config: gpt,
    turns: [
      call("todowrite", {
        todos: [
          { content: "Read the code", status: "completed" },
          { content: 'Fix "add"', status: "in_progress" },
          { content: "Test", status: "pending" },
          { content: "Skip", status: "cancelled" },
        ],
      }),
      call("todowrite", { todos: [] }),
      call("todowrite", { todos: [{ content: "x", status: "bogus" }] }),
      say("Done."),
    ],
  },
  (ctx) => turn(ctx, "Plan it"),
)

scenario(
  "patch: asks for the edit",
  {
    files,
    config: { ...gpt, permissions: [...baseConfig.permissions, { action: "edit", resource: "*", effect: "ask" }] },
    turns: [call("patch", { patchText: cases["add update delete and move"]! }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    let from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "Patch it" })
    let state = await ctx.settle(id, from)
    while (state === "permission.asked") {
      const pending = await ctx.http("GET", `/api/session/${id}/permission`)
      from = ctx.events.length
      await ctx.http("POST", `/api/session/${id}/permission/${pending.body.data[0].id}/reply`, { decision: "once" })
      state = await ctx.settle(id, from)
    }
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
