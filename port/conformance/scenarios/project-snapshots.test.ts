// Git-backed snapshots of a project: each model step in a git checkout captures the working tree, so the files a turn
// changed, per-turn diffs and reverts reflect the repository (ignore rules, oversized untracked files, nested
// repositories, deletions and renames made by tools).
import { spawnSync } from "node:child_process"
import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { call, say } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

/** Prepare repository state with the git CLI, like the harness's `git: true` option (scenario input). */
function sh(ctx: Ctx, command: string) {
  const result = spawnSync("sh", ["-c", command], { cwd: ctx.project, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr}`)
}

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
  ".gitignore": "dist/\n*.log\n",
}

/** User message IDs of a session, oldest first. */
async function userMessages(ctx: Ctx, sessionID: string) {
  const result = await ctx.http("GET", `/api/session/${sessionID}/message`)
  return (result.body.data as Array<{ id: string; type: string }>).filter((m) => m.type === "user").map((m) => m.id).reverse()
}

scenario(
  "turn diffs and file lists from snapshots",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "src/new.ts", content: "export const fresh = 1\n" }),
      call("edit", { path: "src/math.ts", oldString: "return a + b", newString: "return b + a" }),
      call("write", { path: "dist/out.js", content: "ignored build output\n" }),
      say("Changed three files."),
      call("write", { path: "notes/todo.txt", content: "one\ntwo\nthree\nfour\n" }),
      say("Added a todo."),
      say("Nothing to change."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Make the changes")
    await ctx.prompt(id, "Add a todo")
    await ctx.prompt(id, "Just answer")
    const [first, second, third] = await userMessages(ctx, id)
    await ctx.http("GET", `/api/session/${id}/diff`)
    await ctx.http("GET", `/api/session/${id}/diff?from=${first}`)
    await ctx.http("GET", `/api/session/${id}/diff?from=${first}&context=0`)
    await ctx.http("GET", `/api/session/${id}/diff?from=${second}`)
    await ctx.http("GET", `/api/session/${id}/diff?from=${first}&to=${third}&context=1`)
    await ctx.http("GET", `/api/session/${id}/diff?from=${third}`)
  },
)

scenario(
  "revert a turn restores files from snapshots",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "src/new.ts", content: "export const fresh = 1\n" }),
      call("write", { path: "README.md", content: "# Rewritten\n" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Rewrite things")
    const [first] = await userMessages(ctx, id)
    await ctx.http("POST", `/api/session/${id}/revert/stage`, { messageID: first, files: true })
    ctx.note("README after staging the revert", ctx.read("README.md"))
    await ctx.http("DELETE", `/api/session/${id}/revert`)
    await ctx.wait(id)
    ctx.note("README after clearing the revert", ctx.read("README.md"))
    await ctx.http("POST", `/api/session/${id}/revert/stage`, { messageID: first, files: true })
    await ctx.http("POST", `/api/session/${id}/revert/commit`)
    await ctx.http("GET", `/api/session/${id}/message`)
  },
)

scenario(
  "snapshots skip oversized untracked files and nested repositories",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "vendor/lib/index.js", content: "module.exports = 2\n" }),
      call("write", { path: "small.txt", content: "small\n" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    // A 3 MiB untracked file exceeds the snapshot size cap; vendor/lib is its own repository.
    writeFileSync(path.join(ctx.project, "big.bin"), Buffer.alloc(3 * 1024 * 1024, 1))
    mkdirSync(path.join(ctx.project, "vendor/lib"), { recursive: true })
    writeFileSync(path.join(ctx.project, "vendor/lib/index.js"), "module.exports = 1\n")
    sh(ctx, "cd vendor/lib && git init -q -b main && git add -A && git commit -q -m vendored")
    sh(ctx, "git add vendor/lib 2>/dev/null; git commit -q -m 'add vendored repo' || true")
    // A tracked file that the ignore rules match is left out of snapshots.
    writeFileSync(path.join(ctx.project, "tracked.log"), "tracked but ignored\n")
    sh(ctx, "git add -f tracked.log && git commit -q -m 'force-add a log'")
    const id = await ctx.session()
    await ctx.prompt(id, "Touch files")
    await ctx.http("GET", `/api/session/${id}/diff`)
  },
)

scenario(
  "snapshots in a session at a nested location",
  {
    git: true,
    files: { ...files, "pkg/a.txt": "a\n" },
    turns: [call("write", { path: "a.txt", content: "a2\n" }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session({ location: { directory: path.join(ctx.project, "pkg") } })
    await ctx.prompt(id, "Change a")
    await ctx.http("GET", `/api/session/${id}/diff`)
  },
)

scenario(
  "turn diffs with quoted file names",
  {
    git: true,
    files,
    turns: [
      call("write", { path: 'quote"d.txt', content: "quoted\n" }),
      call("write", { path: "tab\tname.txt", content: "tabbed\n" }),
      call("write", { path: "with space.txt", content: "spaced\n" }),
      call("write", { path: "ünïcode.txt", content: "unicode\n" }),
      // NUL bytes make git treat these as binary: their patch headers carry no ---/+++ lines.
      call("write", { path: 'bin"ary.dat', content: "\u0000\u0001binary" }),
      call("write", { path: "plain.dat", content: "\u0000\u0002binary" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Create oddly named files")
    await ctx.http("GET", `/api/session/${id}/diff`)
  },
)

scenario(
  "revert restores non-ASCII paths one by one",
  {
    git: true,
    files,
    turns: [
      call("write", { path: "ünïcode.txt", content: "new file\n" }),
      call("write", { path: "README.md", content: "# Changed\n" }),
      say("Done."),
    ],
  },
  async (ctx) => {
    const id = await ctx.session()
    await ctx.prompt(id, "Change files")
    const [first] = await userMessages(ctx, id)
    await ctx.http("POST", `/api/session/${id}/revert/stage`, { messageID: first, files: true })
    await ctx.http("POST", `/api/session/${id}/revert/commit`)
  },
)
