// The global config patch (PATCH /api/experimental/config) edits the global config file in place, as jsonc-parser's
// `modify` with its formatter does it (packages/core/src/config.ts `Config.update`): the property is found, set or
// removed, and the lines the edit touched are reformatted, whatever the rest of the file looks like. Each scenario
// writes the global file, patches it, and records the file the server left.
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

// The global config directory is shared by every scenario in the process: whatever a scenario writes there is removed
// again so later scenarios see the same starting point.
const globalConfigDir = () => path.join(process.env.XDG_CONFIG_HOME!, "opencode")
const names = ["opencode.json", "opencode.jsonc"]
const resetGlobalConfig = () => {
  for (const name of names) rmSync(path.join(globalConfigDir(), name), { force: true })
}
const recordFiles = (ctx: Ctx) => {
  for (const name of names) {
    const file = path.join(globalConfigDir(), name)
    if (existsSync(file)) ctx.note(name, readFileSync(file, "utf8"))
  }
}

/**
 * Writes the global config files, patches the shell (one patch per entry), and records the files after each. A patch
 * waits for the reload it triggers (`config.updated`), unless `updated: false`: the patch fails, or the file it
 * leaves does not load, so the reload changes nothing and announces nothing.
 */
const patchScenario = (
  name: string,
  files: Record<string, string>,
  patches: readonly (string | null)[],
  options: { readonly config?: boolean; readonly updated?: boolean } = {},
) =>
  // The file watcher is off: its reload of the global directory would race the reload the patch triggers.
  scenario(name, { server: { filewatcher: false } }, async (ctx) => {
    resetGlobalConfig()
    try {
      mkdirSync(globalConfigDir(), { recursive: true })
      for (const [file, text] of Object.entries(files)) writeFileSync(path.join(globalConfigDir(), file), text)
      for (const shell of patches) {
        const from = ctx.events.length
        await ctx.http("PATCH", "/api/experimental/config", { shell })
        if (options.updated !== false) await ctx.until((event) => event?.type === "config.updated", from)
        recordFiles(ctx)
      }
      if (options.config) await ctx.http("GET", "/api/config")
    } finally {
      resetGlobalConfig()
    }
  })

patchScenario(
  "a shell appended to a commented jsonc file with tabs and a trailing comma",
  { "opencode.jsonc": '// global settings\n{\n\t"username": "dev", // me\n\t/* sharing */ "share": "manual",\n}\n' },
  ["/bin/zsh", "/usr/bin/fish"],
  { config: true },
)

patchScenario(
  "a shell set in an inline json file, then replaced and removed",
  { "opencode.json": '{"username":"dev","share":"manual"}' },
  ["/bin/sh", "C:\\Program Files\\Git\\bin\\bash.exe", null],
  { config: true },
)

patchScenario(
  "a shell replaced in place keeps the rest of the line",
  { "opencode.jsonc": '{\n    "username" :"dev" ,   "shell"  :   "/bin/sh"  ,"share":"manual"\n}\n' },
  ['say "hi" ü 😀'],
)

patchScenario(
  "the shell removed from a CRLF file, then the first and only property",
  { "opencode.jsonc": '{\r\n    "username": "dev",\r\n    "shell": "/bin/sh", // gone\r\n    "share": "manual"\r\n}\r\n' },
  [null, "/bin/bash", null],
)

patchScenario(
  "the first property removed before others on one line",
  { "opencode.jsonc": '{ "shell": "/bin/sh", /* kept */ "username": "dev" }' },
  [null],
)

patchScenario(
  "the only property removed collapses the lines it was on",
  { "opencode.json": '{\n  // the shell\n  "shell": "/bin/sh"\n}\n' },
  [null],
)

patchScenario("a shell removed from a file without one leaves it unchanged", { "opencode.jsonc": "{\n\t// nothing\n}" }, [
  null,
])

patchScenario("a shell set in an empty file", { "opencode.jsonc": "" }, ["/bin/sh"], { config: true })

patchScenario("a shell set in a file of comments only", { "opencode.jsonc": "// TODO\n/* settings */\n" }, ["/bin/sh"])

patchScenario("a shell set in a file of carriage returns", { "opencode.jsonc": '{\r  "username": "dev"\r}\r' }, ["/bin/sh"])

patchScenario(
  "the first of duplicate shell keys is edited and the last still wins",
  { "opencode.jsonc": '{\n  "shell": "/bin/a",\n  "shell": "/bin/b"\n}\n' },
  ["/bin/zsh"],
  { config: true },
)

patchScenario("an escaped key names the shell", { "opencode.jsonc": '{ "sh\\u0065ll": "/bin/sh" }\n' }, ["/bin/bash"], {
  config: true,
})

patchScenario(
  "a shell set in a nested and oddly indented file",
  {
    "opencode.jsonc":
      '   {\n "tool_output": {\n\t\t"max_lines": 50,\n      "max_bytes": 4096 },\n        "username": "dev" /* last */\n}\n',
  },
  ["/bin/sh"],
  { config: true },
)

patchScenario(
  "a shell set in an unreadable file is written in front of it",
  { "opencode.jsonc": "not json at all" },
  ["/bin/sh"],
  { config: true, updated: false },
)

patchScenario(
  "a shell set in a file missing its closing brace",
  { "opencode.jsonc": '{\n  "username": "dev",\n' },
  ["/bin/sh"],
  { config: true, updated: false },
)

patchScenario("a file whose root is an array fails the patch", { "opencode.jsonc": '["shell"]\n' }, ["/bin/sh", null], {
  updated: false,
})

patchScenario("removing from an empty file fails the patch", { "opencode.json": "  \n" }, [null], { updated: false })

patchScenario(
  "with both global files the later name is edited",
  { "opencode.json": '{ "username": "json" }\n', "opencode.jsonc": '{ "username": "jsonc" }\n' },
  ["/bin/sh"],
  { config: true },
)
