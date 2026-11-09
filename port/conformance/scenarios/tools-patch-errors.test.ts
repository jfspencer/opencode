// The patch tool when the file system refuses: its message carries the system error's own text (Node's
// `EACCES: permission denied, open '<path>'`, through tool/plugin/patch.ts `errorMessage` and the environment's
// node:fs calls in environment/local.ts), with the call it came from (`stat`, `open`, `mkdir`, `rm`) and the path
// that call was given. Permissions are changed before the prompt and restored before the files are recorded.
import { chmodSync, statSync } from "node:fs"
import path from "node:path"
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
}
const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n    return a + b\n}\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
  "notes/keep.txt": "keep\n",
}
const patch = (...body: string[]) => ["*** Begin Patch", ...body, "*** End Patch"].join("\n")

// [patch, paths to lock with their modes]
const cases: Record<string, [string, Record<string, number>]> = {
  "an unreadable file": [patch("*** Update File: src/math.ts", "-    return a + b", "+    return b + a"), { "src/math.ts": 0o000 }],
  "a file in an unsearchable directory": [
    patch("*** Update File: notes/todo.txt", " one", "-two", "+TWO"),
    { notes: 0o000 },
  ],
  "deleting in a read-only directory": [patch("*** Delete File: notes/todo.txt"), { notes: 0o555 }],
  "adding to a read-only directory": [patch("*** Add File: notes/new.txt", "+new"), { notes: 0o555 }],
  "adding below a read-only directory": [patch("*** Add File: notes/a/b/new.txt", "+new"), { notes: 0o555 }],
  "adding below a file": [patch("*** Add File: README.md/inner/new.txt", "+new"), {}],
  "updating a read-only file": [patch("*** Update File: src/math.ts", "-    return a + b", "+    return b + a"), { "src/math.ts": 0o444 }],
  "moving out of a read-only directory": [
    patch(
      "*** Add File: added.txt",
      "+first",
      "*** Update File: notes/todo.txt",
      "*** Move to: moved/done.txt",
      " one",
      "-two",
      "+TWO",
    ),
    { notes: 0o555 },
  ],
  "a later change failing": [
    patch(
      "*** Update File: README.md",
      "-A small project.",
      "+A patched project.",
      "*** Delete File: notes/keep.txt",
    ),
    { notes: 0o555 },
  ],
}

for (const [name, [patchText, locks]] of Object.entries(cases))
  scenario(`patch: ${name}`, { files, config: gpt, turns: [call("patch", { patchText }), say("Done.")] }, async (ctx: Ctx) => {
    const lock = (restore: boolean) =>
      Object.entries(locks).forEach(([rel, locked]) => {
        const file = path.join(ctx.project, rel)
        chmodSync(file, !restore ? locked : statSync(file).isDirectory() ? 0o755 : 0o644)
      })
    lock(false)
    try {
      const id = await ctx.session()
      await ctx.prompt(id, "Patch it")
      await ctx.http("GET", `/api/session/${id}/message`)
    } finally {
      lock(true)
    }
  })
