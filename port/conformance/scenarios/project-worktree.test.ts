// Worktrees: the saved inventory, creating worktrees (explicit and configured parent directories, refs, name
// collisions, the project's start command), removing them (dirty checkouts need force), and refreshing the inventory
// from what git reports.
import { spawnSync } from "node:child_process"
import { rmSync } from "node:fs"
import path from "node:path"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
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

/** Boot the project location and return its project ID. */
async function project(ctx: Ctx) {
  const result = await ctx.http("GET", "/api/location")
  await ctx.until((event) => event?.type === "plugin.updated")
  return result.body.project.id as string
}

const files = { "README.md": "# Demo\n", "src/app.ts": "export const app = 1\n" }

// File-watcher reloads of a freshly created worktree race its first boot (in TS itself), so this scenario runs
// without the watcher; its subject is the worktree API.
scenario("create, list and remove worktrees", { git: true, files, server: { filewatcher: false } }, async (ctx) => {
  const projectID = await project(ctx)
  const parent = path.join(ctx.project, ".wt")
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  const created = await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "first" })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "first" })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  const from = ctx.events.length
  await ctx.http("GET", "/api/location", undefined, { "x-opencode-directory": encodeURIComponent(created.body.directory) })
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === created.body.directory, from)
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: created.body.directory, force: false })
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: created.body.directory, force: false })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
})

scenario("create a worktree at a ref and from a linked worktree", { git: true, files }, async (ctx) => {
  ctx.write("second.txt", "2\n")
  sh(ctx, "git add -A && git commit -q -m second && git branch older HEAD~1")
  const projectID = await project(ctx)
  const parent = path.join(ctx.project, ".wt")
  const old = await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "old", branch: "older" })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "missing-ref", branch: "no-such-ref" })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "nested", from: old.body.directory })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "stray", from: path.join(ctx.project, "src") })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "blank", branch: "   " })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
})

scenario("removing a dirty worktree requires force", { git: true, files }, async (ctx) => {
  const projectID = await project(ctx)
  const created = await ctx.http("POST", "/api/worktree", { projectID, directory: path.join(ctx.project, ".wt"), name: "dirty" })
  ctx.write(".wt/dirty/README.md", "# Changed in the worktree\n")
  ctx.write(".wt/dirty/untracked.txt", "new\n")
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: created.body.directory, force: false })
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: created.body.directory, force: true })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: ctx.project, force: true })
  await ctx.http("DELETE", "/api/worktree", { projectID, directory: path.join(ctx.project, "nowhere"), force: true })
})

scenario("worktree start command and name collisions", { git: true, files }, async (ctx) => {
  const projectID = await project(ctx)
  await ctx.http("PATCH", `/api/project/${projectID}`, {
    commands: { start: 'echo "base=${OPENCODE_WORKTREE_BASE#$OPENCODE_WORKTREE_PATH}" > started.txt' },
  })
  const parent = path.join(ctx.project, ".wt")
  ctx.write(".wt/taken/placeholder.txt", "occupied\n")
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "taken" })
  await ctx.http("PATCH", `/api/project/${projectID}`, { commands: { start: "exit 3" } })
  await ctx.http("POST", "/api/worktree", { projectID, directory: parent, name: "failing" })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
})

scenario("worktree directory from config", { git: true, files: { ...files, "opencode.json": JSON.stringify({ worktree: { directory: "configured-wt" } }) } }, async (ctx) => {
  const projectID = await project(ctx)
  await ctx.http("POST", "/api/worktree", { projectID, name: "from-config" })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
})

scenario("refresh discovers and forgets worktrees", { git: true, files }, async (ctx) => {
  const projectID = await project(ctx)
  // One discovery per refresh: rows saved by the same refresh share a creation time, which orders the inventory.
  sh(ctx, "git worktree add -q --detach .wt/gone HEAD")
  await ctx.http("POST", "/api/worktree/refresh", { projectID })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  sh(ctx, "git worktree add -q --detach .wt/manual HEAD")
  await ctx.http("POST", "/api/worktree/refresh", { projectID })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  rmSync(path.join(ctx.project, ".wt/gone"), { recursive: true, force: true })
  await ctx.http("POST", "/api/worktree/refresh", { projectID })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  await ctx.http("POST", "/api/worktree/refresh", { projectID })
})

scenario("worktree routes for unknown projects and non-git projects", { files }, async (ctx) => {
  const projectID = await project(ctx)
  await ctx.http("GET", "/api/worktree?projectID=unknown-project")
  await ctx.http("POST", "/api/worktree", { projectID: "unknown-project", name: "x" })
  await ctx.http("DELETE", "/api/worktree", { projectID: "unknown-project", directory: ctx.project, force: false })
  await ctx.http("POST", "/api/worktree/refresh", { projectID: "unknown-project" })
  await ctx.http("GET", `/api/worktree?projectID=${projectID}`)
  await ctx.http("POST", "/api/worktree", { projectID, directory: path.join(ctx.project, ".wt"), name: "plain" })
  await ctx.http("POST", "/api/worktree/refresh", { projectID })
  await ctx.http("POST", "/api/worktree", { projectID, directory: "relative/dir", name: "bad" })
})
