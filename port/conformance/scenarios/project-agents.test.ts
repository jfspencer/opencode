// Agents: the built-in registry, custom agents from config `agents` and from markdown files in config directories,
// and how a selected agent's prompt, permissions, request overlays and model reach the provider request.
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { baseConfig, MODEL_REF } from "../lib/model"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

scenario("built-in agents", {}, async (ctx) => {
  await ctx.http("GET", "/api/agent")
  await ctx.http("GET", "/api/agent/build")
  await ctx.http("GET", "/api/agent/plan")
  await ctx.http("GET", "/api/agent/does-not-exist")
})

// Each lookup is the first request to its own location, so it must wait for the location's plugins: they register
// the agents and apply the config to them. (A provider lookup does not wait, on purpose upstream: on a cold location
// it races the plugins, so it has no place here.)
scenario(
  "agent lookups on a location's first request",
  {
    boot: false,
    files: { "sub/a.txt": "a\n" },
    config: { agents: { build: { description: "Configured build.", color: "#123456" } } },
  },
  async (ctx) => {
    const first = async (url: string, directory: string) => {
      const from = ctx.events.length
      await ctx.http("GET", url, undefined, { "x-opencode-directory": encodeURIComponent(directory) })
      await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory, from)
    }
    await first("/api/agent/build", ctx.project)
    await first("/api/agent/general", path.join(ctx.project, "sub"))
  },
)

scenario(
  "custom agents from config",
  {
    config: {
      agents: {
        reviewer: {
          description: "Reviews code without editing it.",
          mode: "primary",
          system: "You are a meticulous code reviewer.",
          color: "#FF8800",
          steps: 3,
          model: `${MODEL_REF}#low`,
          request: { headers: { "x-review": "yes" }, body: { temperature: 0.1 } },
          permissions: [
            { action: "edit", resource: "*", effect: "deny" },
            { action: "read", resource: "~/secrets/**", effect: "deny" },
            { action: "external_directory", resource: "$HOME/shared", effect: "allow" },
            { action: "shell", resource: "~/bin/*", effect: "ask" },
          ],
        },
        helper: { description: "A hidden helper subagent.", mode: "subagent", hidden: true, color: "red" },
        plan: { description: "Overridden plan description." },
        general: { disabled: true },
      },
      permissions: [...baseConfig.permissions, { action: "read", resource: "~", effect: "allow" }],
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
    await ctx.http("GET", "/api/agent/reviewer")
    await ctx.http("GET", "/api/agent/helper")
    await ctx.http("GET", "/api/agent/general")
  },
)

scenario(
  "default agent from config",
  { config: { default_agent: "plan" } },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
    const id = await ctx.session()
    await ctx.http("GET", `/api/session/${id}`)
  },
)

scenario(
  "default agent that is a subagent falls back to build",
  { config: { default_agent: "explore" } },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
  },
)

scenario(
  "without build or a configured default the first selectable agent leads",
  { config: { agents: { build: { disabled: true }, plan: { hidden: true }, solo: { mode: "primary", description: "Solo." } } } },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
  },
)

scenario(
  "no selectable primary agent",
  { config: { agents: { build: { disabled: true }, plan: { mode: "subagent" } } } },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
  },
)

scenario(
  "agents from markdown files",
  {
    files: {
      ".opencode/agents/reviewer.md": [
        "---",
        "description: Reviews changes from a markdown file.",
        "mode: subagent",
        "color: \"#00AA00\"",
        "steps: 2",
        "permissions:",
        "  - action: edit",
        "    resource: \"*\"",
        "    effect: deny",
        "---",
        "",
        "You review code. Be terse.",
        "",
      ].join("\n"),
      ".opencode/agent/team/lead.md": [
        "---",
        "description: Nested agent name from its path.",
        "model: openrouter/deepseek/deepseek-v4.1-flash-1017",
        "variant: high",
        "---",
        "Lead the team.",
      ].join("\n"),
      ".opencode/modes/focus.md": ["---", "description: A primary mode.", "---", "Stay focused."].join("\n"),
      ".opencode/agents/broken.md": ["---", "steps: -4", "description: invalid steps", "---", "Broken."].join("\n"),
      ".opencode/agents/extra.md": [
        "---",
        "description: Has an unknown field next to a native one.",
        "system: ignored because the body wins",
        "flavor: vanilla",
        "---",
        "Extra body.",
      ].join("\n"),
      ".opencode/agents/colon.md": ["---", "description: Uses: an unquoted colon", "---", "Colon body."].join("\n"),
      ".opencode/agents/not-markdown.txt": "ignored",
      ".opencode/agents/unparseable.md": ["---", "description: [unclosed", "---", "Body."].join("\n"),
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/agent")
    await ctx.http("GET", "/api/agent/team%2Flead")
  },
)

scenario(
  "a custom agent shapes the provider request",
  {
    files: { "notes.txt": "hello\n" },
    config: {
      agents: {
        reviewer: {
          description: "Reviews code without editing it.",
          mode: "primary",
          system: "You are a meticulous code reviewer. Never edit files.",
          model: `${MODEL_REF}#low`,
          request: { headers: { "x-review": "yes" }, body: { top_p: 0.5 } },
          permissions: [
            { action: "edit", resource: "*", effect: "deny" },
            { action: "write", resource: "*", effect: "deny" },
            { action: "shell", resource: "*", effect: "deny" },
          ],
        },
      },
    },
    turns: [call("read", { path: "notes.txt" }), say("Reviewed.")],
  },
  async (ctx) => {
    const id = await ctx.session({ agent: "reviewer" })
    await ctx.prompt(id, "Review notes.txt")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "a markdown agent prompt reaches the provider",
  {
    files: {
      ".opencode/agents/poet.md": ["---", "description: Writes poems.", "mode: primary", "---", "Answer only in haiku."].join(
        "\n",
      ),
    },
    turns: [say("Old pond, frog jumps in.")],
  },
  async (ctx) => {
    const id = await ctx.session({ agent: "poet" })
    await ctx.prompt(id, "Describe a pond.")
  },
)

scenario(
  "switching a session to a custom agent",
  {
    config: { agents: { quiet: { description: "Quiet agent.", mode: "primary", system: "Reply in one word." } } },
    turns: [say("Hello there."), say("Hi.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Greet me.")
    await ctx.http("POST", `/api/session/${id}/agent`, { agent: "quiet" })
    await ctx.prompt(id, "Greet me again.")
    await ctx.http("POST", `/api/session/${id}/agent`, { agent: "missing-agent" })
  },
)
