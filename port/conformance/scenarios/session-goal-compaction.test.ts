// The goal loop across a compaction (core/src/plugin/goal.ts, the `context` hook): once a compaction leaves the
// objective out of the history a step sends, the step's request gets a reminder of it near its tail, until the next
// continuation restates it. The summary request is an ordinary chat request the script answers like any other turn.
import { say, type Turn } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

/** A reported prompt past the automatic compaction threshold (85% of the 1,048,576-token window). */
const FULL = { prompt: 900_000, completion: 10 }
const SUMMARY = say("## Objective\n- Read the project\n\n## Work State\n### Completed\n- Read the README\n\n## Next Move\n1. Read the math module")
const files = { "README.md": "# Demo\n\nA small project.\n", "src/math.ts": "export const add = (a: number, b: number) => a + b\n" }
const verdict = (value: Record<string, unknown>): Turn => say(JSON.stringify(value))

scenario(
  "a goal is restated after a compaction within its round",
  {
    files,
    turns: [
      { events: [{ text: "Reading." }, { tool: "read", input: { path: "README.md" } }], usage: FULL },
      SUMMARY,
      say("Read the README."),
      verdict({ verdict: "continue", reason: "The math module is unread.", next_step: "Read src/math.ts" }),
      { events: [{ tool: "read", input: { path: "src/math.ts" } }] },
      say("Read both."),
      verdict({ verdict: "done", reason: "Both files were read." }),
    ],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "goal" })
    await ctx.http("POST", `/api/session/${id}/command`, { name: "goal", text: "Read the README and the math module" })
    await ctx.settle(id)
    await ctx.http("POST", "/api/rpc/opencode.goal/get", { input: { sessionID: id } })
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
