// Version control: repository info, review base, working-copy status, branches and diffs (working, branch,
// committed), and initializing a repository in a markerless project.
//
// Repository state is prepared with the git CLI directly, like the harness's own `git: true` option: it is scenario
// input (project files), not an observation. (The session shell endpoint would also work, but its events carry the
// child process pid, which is not normalized.)
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

/** Run a shell command (git, printf) in the project directory to prepare repository state. */
async function sh(ctx: Ctx, command: string) {
  const result = spawnSync("sh", ["-c", command], { cwd: ctx.project, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr}`)
}

/** Rebuild loaded locations so branch information is re-read (the oracle runs without a file watcher). */
async function reload(ctx: Ctx) {
  await ctx.until((event) => event?.type === "plugin.updated")
  const from = ctx.events.length
  await ctx.http("POST", "/api/location/reload")
  await ctx.until((event) => event?.type === "plugin.updated", from)
}

/**
 * The VCS routes (except init) don't wait for plugin activation, and the git provider is registered by a plugin:
 * boot the location and let its plugins activate before asking, or the answers race the provider registration.
 */
async function boot(ctx: Ctx, headers?: Record<string, string>) {
  await ctx.http("GET", "/api/location", undefined, headers)
  await ctx.until((event) => event?.type === "plugin.updated")
}

async function inspect(ctx: Ctx, diffs: string[] = ["working"]) {
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/base")
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/branch")
  for (const mode of diffs) await ctx.http("GET", `/api/vcs/diff?mode=${mode}`)
}

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export function add(a: number, b: number) {\n  return a + b\n}\n",
  "src/old-name.ts": "export const legacy = true\n",
  "notes/todo.txt": "one\ntwo\nthree\n",
  ".gitignore": "dist/\n*.log\n",
}

scenario("vcs routes outside a repository", { files: { "a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx)
  await inspect(ctx, ["working", "branch", "committed"])
})

scenario("vcs routes on a clean repository", { git: true, files }, async (ctx) => {
  await boot(ctx)
  await inspect(ctx, ["working", "branch", "committed"])
})

// The first request to a location boots it, and the answers wait for its plugins to register the git provider.
scenario("vcs routes on a location's first request", { boot: false, git: true, files }, async (ctx) => {
  ctx.write("src/new.ts", "export const fresh = 1\n")
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/branch")
})

scenario("working copy status and diff", { git: true, files }, async (ctx) => {
  ctx.write("src/math.ts", "export function add(a: number, b: number) {\n  return b + a\n}\n\nexport const zero = 0\n")
  ctx.write("src/new.ts", "export const fresh = 1\n")
  ctx.write("dist/bundle.js", "ignored output\n")
  ctx.write("debug.log", "ignored log\n")
  rmSync(path.join(ctx.project, "notes/todo.txt"))
  await sh(ctx, "git mv src/old-name.ts src/new-name.ts && git add src/new-name.ts")
  ctx.write("staged.txt", "staged\n")
  await sh(ctx, "git add staged.txt")
  await boot(ctx)
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/diff?mode=working")
  await ctx.http("GET", "/api/vcs/diff?mode=working&context=0")
  await ctx.http("GET", "/api/vcs/diff?mode=working&context=1&untracked=folders")
  await ctx.http("GET", "/api/vcs/diff?mode=working&untracked=files")
})

scenario("binary, empty and special file names in the diff", { git: true, files }, async (ctx) => {
  ctx.write("empty.txt", "")
  ctx.write("with space.txt", "spaced\n")
  ctx.write("tab\tname.txt", "tabbed\n")
  ctx.write('quote"d.txt', "quoted\n")
  ctx.write("ünïcode.txt", "unicode\n")
  await sh(ctx, "printf '\\000\\001\\002binary' > blob.bin && printf 'no newline' > nonl.txt")
  await boot(ctx)
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/diff?mode=working")
  await sh(ctx, "git add -A && git commit -q -m special")
  ctx.write("with space.txt", "spaced again\n")
  await sh(ctx, "printf '\\000\\003changed' > blob.bin && git rm -q --cached empty.txt")
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/diff?mode=working")
})

scenario("feature branch review base and diffs", { git: true, files }, async (ctx) => {
  await sh(ctx, "git checkout -q -b feature/login")
  ctx.write("src/login.ts", "export const login = () => true\n")
  ctx.write("README.md", "# Demo\n\nA small project with login.\n")
  await sh(ctx, "git add -A && git commit -q -m 'add login'")
  ctx.write("src/login.ts", "export const login = () => false\n")
  ctx.write("scratch.txt", "untracked scratch\n")
  await boot(ctx)
  await inspect(ctx, ["working", "branch", "committed"])
  await ctx.http("GET", "/api/vcs/diff?mode=branch&base=main")
  await ctx.http("GET", "/api/vcs/diff?mode=committed&base=main&context=0")
  await ctx.http("GET", "/api/vcs/diff?mode=branch&base=does-not-exist")
  await ctx.http("GET", "/api/vcs/diff?mode=sideways")
})

scenario("review base inferred from the branch it was created from", { git: true, files }, async (ctx) => {
  await sh(ctx, "git branch develop main && git checkout -q -b feature/api develop")
  ctx.write("src/api.ts", "export const api = 1\n")
  await sh(ctx, "git add -A && git commit -q -m api")
  await boot(ctx)
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/base")
  await ctx.http("GET", "/api/vcs/diff?mode=committed")
  await ctx.http("GET", "/api/vcs/diff?mode=committed&base=develop")
})

scenario("untracked folders collapse in folder mode", { git: true, files }, async (ctx) => {
  ctx.write("newdir/a.txt", "a\n")
  ctx.write("newdir/deep/b.txt", "b\n")
  ctx.write("src/extra.ts", "export {}\n")
  await boot(ctx)
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/diff?mode=working&untracked=folders")
  await ctx.http("GET", "/api/vcs/diff?mode=working&untracked=files")
})

scenario("branch listing with search and limit", { git: true, files }, async (ctx) => {
  await sh(
    ctx,
    "git branch alpha && git branch beta && git branch feature/one && git branch feature/two && git branch Release-1",
  )
  await boot(ctx)
  await ctx.http("GET", "/api/vcs/branch")
  await ctx.http("GET", "/api/vcs/branch?search=feature")
  await ctx.http("GET", "/api/vcs/branch?search=%20RELEASE%20")
  await ctx.http("GET", "/api/vcs/branch?limit=2")
  await ctx.http("GET", "/api/vcs/branch?limit=0")
  await ctx.http("GET", "/api/vcs/branch?search=nothing-matches")
})

scenario("detached head and branch switches", { git: true, files }, async (ctx) => {
  ctx.write("second.txt", "2\n")
  await sh(ctx, "git add -A && git commit -q -m second && git checkout -q --detach HEAD~1")
  await boot(ctx)
  await inspect(ctx, ["working", "branch"])
  await sh(ctx, "git checkout -q main && git checkout -q -b topic")
  await reload(ctx)
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/base")
})

scenario("repository without commits", { files: { "draft.txt": "draft\n" } }, async (ctx) => {
  await sh(ctx, "git init -q -b trunk")
  await boot(ctx)
  await inspect(ctx, ["working", "branch"])
})

scenario("initialize a repository", { files: { "app.txt": "app\n" } }, async (ctx) => {
  await boot(ctx)
  await ctx.http("GET", "/api/vcs")
  await ctx.http("POST", "/api/vcs/init?provider=svn")
  await ctx.until((event) => event?.type === "plugin.updated")
  const from = ctx.events.length
  await ctx.http("POST", "/api/vcs/init")
  // Initialization invalidates the location; the next request boots it again as a git checkout.
  await ctx.http("GET", "/api/location")
  await ctx.until((event) => event?.type === "plugin.updated", from)
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("POST", "/api/vcs/init")
})

scenario("initialize with a provider whose tool is unavailable", { files: { "app.txt": "app\n" } }, async (ctx) => {
  await ctx.http("POST", "/api/vcs/init?provider=hg")
})

// Without the file watcher: with it, vcs routes can answer from state the watcher has not refreshed yet (racy in TS).
scenario("vcs routes from a nested location", { git: true, files, server: { filewatcher: false } }, async (ctx) => {
  ctx.write("src/math.ts", "export const changed = true\n")
  ctx.write("notes/new.txt", "new\n")
  const nested = { "x-opencode-directory": encodeURIComponent(path.join(ctx.project, "src")) }
  await boot(ctx, nested)
  await ctx.http("GET", "/api/vcs/status", undefined, nested)
  await ctx.http("GET", "/api/vcs/diff?mode=working", undefined, nested)
  await ctx.http("GET", "/api/location", undefined, nested)
})
