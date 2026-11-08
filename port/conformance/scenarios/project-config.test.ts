// Configuration documents: how project files (opencode.json, opencode.jsonc, .opencode/, parent directories) layer
// over the base config, variable substitution, rejected documents, and the global config patch endpoint.
import { rmSync } from "node:fs"
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

// The global config directory is shared by every scenario in the process: anything a scenario writes there through
// PATCH /api/experimental/config is removed again so later scenarios see the same starting point.
const globalConfigDir = () => path.join(process.env.XDG_CONFIG_HOME!, "opencode")
const resetGlobalConfig = () => {
  for (const name of ["opencode.json", "opencode.jsonc"]) rmSync(path.join(globalConfigDir(), name), { force: true })
}

scenario("config documents without project files", {}, async (ctx) => {
  await ctx.http("GET", "/api/config")
})

scenario("config documents with project config disabled", { projectConfig: false, files: { "opencode.json": '{"username":"ignored"}' } }, async (ctx) => {
  await ctx.http("GET", "/api/config")
})

scenario(
  "project config files layer in priority order",
  {
    files: {
      "opencode.json": JSON.stringify({ username: "from-json", share: "manual", default_agent: "plan" }),
      "opencode.jsonc": "// comments are allowed\n{\n  \"username\": \"from-jsonc\",\n  \"timezone\": \"Europe/Paris\", // trailing comment\n}\n",
      ".opencode/opencode.json": JSON.stringify({ username: "from-dot-opencode", snapshots: false }),
      ".opencode/opencode.jsonc": "{ \"tool_output\": { \"max_lines\": 50, \"max_bytes\": 4096 }, }",
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/config")
    await ctx.http("GET", "/api/agent")
  },
)

scenario(
  "config from parent directories of a nested location",
  {
    files: {
      "opencode.json": JSON.stringify({ username: "root", instructions: ["ROOT.md"] }),
      ".opencode/opencode.json": JSON.stringify({ timezone: "UTC" }),
      "packages/app/opencode.json": JSON.stringify({ username: "app" }),
      "packages/app/.opencode/opencode.jsonc": "{ \"share\": \"disabled\" }",
      "packages/app/src/.gitkeep": "",
    },
  },
  async (ctx) => {
    const directory = path.join(ctx.project, "packages/app/src")
    const nested = { "x-opencode-directory": encodeURIComponent(directory) }
    await ctx.http("GET", "/api/config", undefined, nested)
    await ctx.http("GET", "/api/location", undefined, nested)
    // The nested location's plugins finish loading after the response: wait for the last of its boot events.
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory)
  },
)

scenario(
  "malformed and non-object config documents are rejected",
  {
    files: {
      "opencode.json": "{ \"username\": \"broken\", ",
      "opencode.jsonc": "[1, 2, 3]",
      ".opencode/opencode.json": "\"just a string\"",
      ".opencode/opencode.jsonc": "{ \"username\": \"survivor\" }",
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/config")
  },
)

scenario(
  "invalid fields are dropped while valid ones are kept",
  {
    files: {
      "opencode.json": JSON.stringify({
        username: 42,
        share: "sometimes",
        update: "never",
        timezone: "Asia/Tokyo",
        snapshots: "yes",
        tool_output: { max_lines: -1 },
        instructions: ["GOOD.md", 7],
        skills: { paths: ["skills-a"], urls: [3] },
        watcher: { ignore: ["dist/**", false] },
        compaction: { auto: false, keep: { tokens: 1000 }, buffer: "20%" },
        formatter: false,
        lsp: false,
        worktree: { directory: "   " },
        agents: { good: { description: "Kept", mode: "subagent" }, bad: { mode: "sideways" } },
        commands: { ok: { template: "Do it" }, broken: { description: "no template" } },
        experimental: { subagent_depth: 2, location_idle_timeout: false, bogus_flag: true },
        logLevel: "DEBUG",
      }),
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/config")
  },
)

scenario(
  "invalid native config sections",
  {
    files: {
      "opencode.json": JSON.stringify({
        skills: "not-a-list",
        watcher: "nope",
        compaction: 5,
        experimental: [],
        mcp: "none",
        formatter: { prettier: { disabled: true }, custom: { command: "fmt" } },
        lsp: { custom: { disabled: true } },
        agents: [],
        commands: "x",
        instructions: "AGENTS.md",
        permissions: { action: "read" },
        references: { docs: 5 },
      }),
      ".opencode/opencode.json": JSON.stringify({
        compaction: { keep: 3, buffer: 0 },
        skills: ["one", "two"],
        watcher: {},
        formatter: {},
        experimental: {},
        compaction_extra: true,
      }),
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/config")
  },
)

scenario(
  "env and file substitution in config",
  {
    files: {
      "secret.txt": "  s3cret-value  \n",
      "nested/quote.txt": 'He said "hi"\nand left\n',
      "opencode.jsonc": [
        "{",
        '  "username": "{file:secret.txt}",',
        '  "timezone": "{env:CONFORMANCE_UNSET_VARIABLE}",',
        '  // "shell": "{file:does-not-exist-but-commented.txt}",',
        '  "default_agent": "{file:./missing.txt}",',
        '  "instructions": ["{env:HOME}/notes.md", "{file:nested/quote.txt}"]',
        "}",
      ].join("\n"),
    },
  },
  async (ctx) => {
    await ctx.http("GET", "/api/config")
  },
)

scenario("available shells", {}, async (ctx) => {
  await ctx.http("GET", "/api/config/shell")
})

// The file watcher is off: its reload of the late .opencode directory would race the reload the patch triggers.
scenario("patch the global config shell", { server: { filewatcher: false } }, async (ctx) => {
  resetGlobalConfig()
  try {
    const from = ctx.events.length
    await ctx.http("PATCH", "/api/experimental/config", { shell: "/bin/sh" })
    await ctx.until((event) => event?.type === "config.updated", from)
    await ctx.http("GET", "/api/config")
    // A project .opencode directory that appears later is picked up by the reload the patch triggers.
    ctx.write(".opencode/opencode.json", JSON.stringify({ username: "late" }))
    const next = ctx.events.length
    await ctx.http("PATCH", "/api/experimental/config", { shell: "/bin/bash" })
    await ctx.until((event) => event?.type === "config.updated", next)
    await ctx.http("GET", "/api/config")
    const last = ctx.events.length
    await ctx.http("PATCH", "/api/experimental/config", { shell: null })
    await ctx.until((event) => event?.type === "config.updated", last)
    await ctx.http("GET", "/api/config")
    await ctx.http("PATCH", "/api/experimental/config", { shell: 5 })
  } finally {
    resetGlobalConfig()
  }
})

scenario(
  "configured formatters run after a write",
  {
    config: {
      formatter: {
        prettier: { disabled: true },
        upper: {
          command: ["sh", "-c", 'tr a-z A-Z < "$0" > "$0.tmp" && mv "$0.tmp" "$0"', "$FILE"],
          extensions: [".up"],
          environment: { CONFORMANCE_FORMATTER: "1" },
        },
        gofmt: { environment: { GOFLAGS: "-mod=mod" } },
      },
    },
    turns: [call("write", { path: "note.up", content: "shout this\n" }), call("write", { path: "plain.txt", content: "keep this\n" }), say("Written.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Write the files")
  },
)
