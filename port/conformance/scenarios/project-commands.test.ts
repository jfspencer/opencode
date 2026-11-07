// Slash commands: the registry (config `commands` and markdown command files), and running one in a session with
// argument templating, shell interpolation, agent/model selection and subagent delivery.
import { say } from "../lib/openrouter"
import { MODEL_REF } from "../lib/model"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

/** Run a command and wait for the session to settle. */
async function command(ctx: Ctx, sessionID: string, name: string, text: string) {
  const from = ctx.events.length
  const result = await ctx.http("POST", `/api/session/${sessionID}/command`, { name, text })
  if (result.status === 204) await ctx.settle(sessionID, from)
  return result
}

const commands = {
  greet: { template: "Say hello to $ARGUMENTS.", description: "Greets someone" },
  pair: { template: "First: $1. Second: $2." },
  rest: { template: "Head $1, tail $2" },
  plain: { template: "Summarize the project." },
  gap: { template: "Only $3 matters" },
}

// GET /api/command does not wait for plugin activation (unlike /api/agent and /api/skill): boot the location and let
// its plugins activate first, or the listing races the config command plugin.
async function listCommands(ctx: Ctx) {
  await ctx.http("GET", "/api/location")
  await ctx.until((event) => event?.type === "plugin.updated")
  await ctx.http("GET", "/api/command")
}

scenario("command list without custom commands", {}, listCommands)

// The first request to a location boots it, and the listing waits for its plugins to register their commands.
scenario("command list on a location's first request", { boot: false, config: { commands } }, async (ctx) => {
  await ctx.http("GET", "/api/command")
})

scenario(
  "commands from config and markdown files",
  {
    config: { commands },
    files: {
      ".opencode/commands/deploy.md": [
        "---",
        "description: Deploy the app",
        "agent: plan",
        "---",
        "Deploy to $1 now.",
      ].join("\n"),
      ".opencode/command/git/commit.md": ["---", "description: Nested command", "---", "Commit with message $ARGUMENTS"].join(
        "\n",
      ),
      ".opencode/commands/bad.md": ["---", "subagent: sometimes", "---", "Bad frontmatter."].join("\n"),
      ".opencode/commands/nofront.md": "Just a template without frontmatter.\n",
      ".opencode/commands/notes.txt": "not a command",
      ".opencode/commands/unparseable.md": ["---", "description: [unclosed", "---", "Body."].join("\n"),
    },
  },
  listCommands,
)

scenario(
  "command argument templating",
  {
    config: { commands },
    turns: [say("One."), say("Two."), say("Three."), say("Four."), say("Five."), say("Six.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await command(ctx, id, "greet", "the whole world")
    await command(ctx, id, "pair", 'alpha "beta gamma" delta')
    await command(ctx, id, "rest", "one two three four")
    await command(ctx, id, "plain", "focus on tests")
    await command(ctx, id, "gap", "a b")
    await command(ctx, id, "plain", "")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "command with shell interpolation",
  {
    config: { commands: { status: { template: "Files:\n!`ls`\nGreeting: !`echo hi $1`" } } },
    files: { "a.txt": "a", "b.txt": "b" },
    turns: [say("Noted.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await command(ctx, id, "status", "there")
  },
)

scenario(
  "command that selects an agent and a model",
  {
    config: {
      commands: {
        review: { template: "Review $ARGUMENTS", agent: "plan", model: `${MODEL_REF}#low` },
        modelonly: { template: "Quick: $ARGUMENTS", model: `${MODEL_REF}#none` },
      },
    },
    turns: [say("Reviewed."), say("Quick answer.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await command(ctx, id, "review", "src/")
    await ctx.http("GET", `/api/session/${id}`)
    await command(ctx, id, "modelonly", "why?")
    await ctx.http("GET", `/api/session/${id}`)
  },
)

scenario(
  "command that runs as a subagent",
  {
    config: { commands: { research: { template: "Research $ARGUMENTS", description: "Research task", subagent: true } } },
    turns: [say("Research findings."), say("Parent acknowledges.")],
    timeout: 60_000,
  },
  async (ctx) => {
    const id = await ctx.session()
    const from = ctx.events.length
    await ctx.http("POST", `/api/session/${id}/command`, { name: "research", text: "caching" })
    const created = await ctx.until((event) => event?.type === "session.created" && event?.data?.sessionID !== id, from)
    const child = created.data.sessionID
    // The child runs first; its completion is delivered to the parent, which then takes its own turn.
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === child, from)
    await ctx.until((event) => event?.type === "session.execution.succeeded" && event?.data?.sessionID === id, from)
    await ctx.wait(id)
    await ctx.http("GET", `/api/session?parentID=${id}`)
    await ctx.http("GET", `/api/session/${child}/message`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "unknown command and missing session",
  { config: { commands } },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.http("POST", `/api/session/${id}/command`, { name: "nope", text: "" })
    await ctx.http("POST", "/api/session/ses_00000000000000000000000000/command", { name: "greet", text: "x" })
  },
)

scenario(
  "command naming an agent that does not exist",
  { config: { commands: { ghost: { template: "Boo", agent: "ghost" } } } },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.http("POST", `/api/session/${id}/command`, { name: "ghost", text: "" })
  },
)
