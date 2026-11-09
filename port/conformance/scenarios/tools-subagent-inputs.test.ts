// The subagent tool's input as the tool runtime decodes it (tool/runtime.ts `decodeInput`): the input repaired
// (plugin/tool-input-repair.ts), empty `model` and `sessionID` dropped by the tool's own `execute.before` hook, then
// decoded against `SubagentTool.Input` with every issue reported as "Invalid arguments". None of these calls reaches
// a child session.
import { call, say } from "../lib/openrouter"
import { scenario } from "../lib/scenario"

const valid = { agent: "general", description: "Look around", prompt: "Look around the project" }

const cases: Record<string, unknown> = {
  "a number for the agent": { ...valid, agent: 1 },
  "every key missing": {},
  "null fields": { agent: null, description: null, prompt: null },
  "a number for the model": { ...valid, model: 5 },
  "a session ID without its prefix": { ...valid, sessionID: "abc" },
  "a number for the session ID": { ...valid, sessionID: 7 },
  "a word for background": { ...valid, background: "yes" },
  "every field wrong": { agent: [], description: {}, prompt: false, model: 1, sessionID: "x", background: 2 },
  "a stringified input with an empty model": JSON.stringify({ ...valid, agent: 3, model: "" }),
  "an array": [valid],
  "a string": "general",
}

for (const [name, input] of Object.entries(cases))
  scenario(
    `subagent input: ${name}`,
    { turns: [{ events: [{ tool: "subagent", rawInput: JSON.stringify(input) }] }, say("Done.")] },
    async (ctx) => {
      const id = await ctx.session()
      await ctx.prompt(id, "Delegate it")
      await ctx.http("GET", `/api/session/${id}/message`)
    },
  )

// Empty `model` and `sessionID` are dropped before decoding, so the rest still reports; the call itself then fails
// on its unknown agent.
scenario(
  "subagent input: empty model and session ID are dropped",
  { turns: [call("subagent", { ...valid, agent: "nobody", model: "", sessionID: "" }), say("Done.")] },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Delegate it")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)
