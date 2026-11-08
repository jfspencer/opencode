// Ambient instructions: AGENTS.md files from the location up to the project root, configured `instructions` (paths,
// globs, absolute and home paths, URLs), and how changes between turns are announced to the model.
import { chmodSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { say } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

/** Rebuild every loaded location so file changes are picked up (the oracle runs without a file watcher). */
async function reload(ctx: Ctx) {
  const from = ctx.events.length
  await ctx.http("POST", "/api/location/reload")
  await ctx.until((event) => event?.type === "plugin.updated", from)
}

scenario(
  "AGENTS.md in the project root",
  {
    files: { "AGENTS.md": "# Rules\n\nAlways answer in English.\n", "src/index.ts": "export {}\n" },
    turns: [say("Understood.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Hello")
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "AGENTS.md files from a nested location up to the git root",
  {
    git: true,
    files: {
      "AGENTS.md": "Root rules.\n",
      "packages/AGENTS.md": "Packages rules.\n",
      "packages/app/AGENTS.md": "App rules.\n",
      "packages/app/src/main.ts": "export const main = 1\n",
      "packages/other/AGENTS.md": "Sibling rules that must not load.\n",
    },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    const id = await ctx.session({ location: { directory: path.join(ctx.project, "packages/app/src") } })
    await ctx.prompt(id, "Where am I?")
  },
)

scenario(
  "configured instruction paths and globs",
  {
    files: {
      "docs/style.md": "Use tabs.\n",
      "docs/naming.md": "Use camelCase.\n",
      "docs/skip.txt": "Not markdown.\n",
      "RULES.md": "Project rules file.\n",
      "nested/deeper/LOCAL.md": "Local rules.\n",
    },
    config: { instructions: ["RULES.md", "docs/*.md", "MISSING.md", "RULES.md"] },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Follow the rules.")
  },
)

scenario(
  "configured instructions resolve upward from a nested location",
  {
    git: true,
    files: {
      "TEAM.md": "Team rules at the root.\n",
      "pkg/TEAM.md": "Team rules in pkg.\n",
      "pkg/lib/code.ts": "export {}\n",
    },
    config: { instructions: ["TEAM.md"] },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    const id = await ctx.session({ location: { directory: path.join(ctx.project, "pkg/lib") } })
    await ctx.prompt(id, "Rules?")
  },
)

scenario(
  "absolute and home instruction paths",
  {
    files: { "abs/ABS.md": "Absolute rules.\n" },
    turns: [say("Ok.")],
  },
  async (ctx) => {
    // A home-relative instruction lives under the sandboxed HOME; it is removed again afterwards.
    const home = path.join(process.env.HOME!, "conformance-instructions")
    mkdirSync(home, { recursive: true })
    writeFileSync(path.join(home, "HOME.md"), "Home rules.\n")
    try {
      // Written before the location boots (the first prompt), so no reload is needed.
      ctx.write(
        "opencode.json",
        JSON.stringify({
          instructions: [
            path.join(ctx.project, "abs/ABS.md"),
            "~/conformance-instructions/HOME.md",
            path.join(ctx.project, "abs/*.md"),
            path.join(ctx.project, "no-such-dir/**/*.md"),
          ],
        }),
      )
      const id = await ctx.session()
      await ctx.prompt(id, "Rules?")
    } finally {
      rmSync(home, { recursive: true, force: true })
    }
  },
)

scenario(
  "remote instruction URLs",
  {
    config: { instructions: ["https://rules.example.test/AGENTS.md", "https://rules.example.test/missing.md"] },
    responders: [
      (request) =>
        request.url === "https://rules.example.test/AGENTS.md"
          ? { raw: "Remote rules: be kind.\n", headers: { "content-type": "text/plain" } }
          : request.url === "https://rules.example.test/missing.md"
            ? { status: 404, body: { error: "not found" } }
            : undefined,
    ],
    turns: [say("Ok.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Rules?")
  },
)

const rules = (changed?: number) =>
  Array.from({ length: 30 }, (_, i) => (i + 1 === changed ? `Rule ${i + 1}: changed.` : `Rule ${i + 1}: keep it simple.`)).join(
    "\n",
  ) + "\n"

scenario(
  "instruction changes between turns",
  {
    // The file watcher is off: the scenario reloads after each write, and the watcher's reload would race it.
    server: { filewatcher: false },
    files: { "AGENTS.md": rules(), "OLD.md": "Old rules.\n" },
    config: { instructions: ["OLD.md", "NEW.md"] },
    turns: [say("First."), say("Second."), say("Third.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Turn one")
    // A one-line edit in a long file is announced as a diff; a rewrite restates the file.
    ctx.write("AGENTS.md", rules(12))
    rmSync(path.join(ctx.project, "OLD.md"))
    ctx.write("NEW.md", "Brand new rules.\n")
    await reload(ctx)
    await ctx.prompt(id, "Turn two")
    ctx.write("AGENTS.md", "Completely different.\n")
    await reload(ctx)
    await ctx.prompt(id, "Turn three")
  },
)

scenario(
  "all instructions removed between turns",
  {
    // The file watcher is off: the scenario reloads after each write, and the watcher's reload would race it.
    server: { filewatcher: false },
    files: { "AGENTS.md": "Temporary rules.\n" },
    turns: [say("First."), say("Second.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Turn one")
    rmSync(path.join(ctx.project, "AGENTS.md"))
    await reload(ctx)
    await ctx.prompt(id, "Turn two")
  },
)

scenario(
  "moving a session changes its environment and instructions",
  {
    git: true,
    files: { "AGENTS.md": "Root.\n", "sub/AGENTS.md": "Sub rules.\n", "sub/file.txt": "x\n" },
    turns: [say("First."), say("Second.")],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Turn one")
    await ctx.http("POST", `/api/session/${id}/move`, { directory: path.join(ctx.project, "sub") })
    // The move is queued: let it run before the next prompt, which would otherwise race it.
    await ctx.until((event) => event?.type === "session.moved" && event?.data?.sessionID === id)
    await ctx.wait(id)
    await ctx.prompt(id, "Turn two")
    await ctx.http("GET", `/api/session/${id}`)
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === path.join(ctx.project, "sub"))
  },
)

scenario("global AGENTS.md from the config directory", { files: { "AGENTS.md": "Project rules.\n" }, turns: [say("Ok.")] }, async (ctx) => {
  // The global config directory is shared by every scenario in the process: remove the file again afterwards.
  const global = path.join(process.env.XDG_CONFIG_HOME!, "opencode", "AGENTS.md")
  mkdirSync(path.dirname(global), { recursive: true })
  writeFileSync(global, "Global rules for every project.\n")
  try {
    const id = await ctx.session()
    await ctx.prompt(id, "Rules?")
  } finally {
    rmSync(global, { force: true })
  }
})

scenario(
  "unreadable AGENTS.md blocks instruction initialization",
  // The file watcher is off: the scenario reloads after the change, and the watcher's reload would race it.
  { server: { filewatcher: false }, files: { "AGENTS.md": "Secret rules.\n" }, turns: [say("Ok."), say("Now readable.")] },
  async (ctx) => {
    chmodSync(path.join(ctx.project, "AGENTS.md"), 0o000)
    try {
      const id = await ctx.session()
      await ctx.prompt(id, "Rules?")
      await ctx.http("GET", `/api/session/${id}/message`)
      chmodSync(path.join(ctx.project, "AGENTS.md"), 0o644)
      await reload(ctx)
      await ctx.prompt(id, "Rules now?")
    } finally {
      // The recorder reads every project file at the end.
      chmodSync(path.join(ctx.project, "AGENTS.md"), 0o644)
    }
  },
)
