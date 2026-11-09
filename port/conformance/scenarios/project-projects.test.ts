// Projects and locations: project identity (directory, git root commit, cached ID, linked worktrees, Mercurial
// markers), the project list and update routes, icon discovery, location get/reload, loaded-location debugging,
// and the small server-level routes (info, forms, permission requests, usage limits).
import { spawnSync } from "node:child_process"
import { mkdirSync, renameSync, writeFileSync } from "node:fs"
import path from "node:path"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
// The first boot of a location is what these scenarios observe, so the harness must not boot it beforehand.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, { boot: false, ...options }, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

/** Prepare repository state with the git CLI, like the harness's `git: true` option (scenario input). */
function sh(ctx: Ctx, command: string, cwd = ctx.project) {
  const result = spawnSync("sh", ["-c", command], { cwd, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr}`)
}

const at = (directory: string) => ({ "x-opencode-directory": encodeURIComponent(directory) })

/** Boot a location and wait until its plugins have activated. */
async function boot(ctx: Ctx, directory = ctx.project) {
  const from = ctx.events.length
  const result = await ctx.http("GET", "/api/location", undefined, at(directory))
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory, from)
  return result
}

/** The project list, restricted to projects this scenario created (the server also knows its own cwd). */
async function projects(ctx: Ctx, query = "") {
  const result = await ctx.http("GET", `/api/project${query}`)
  return result.body as Array<{ id: string; canonical: string }>
}

scenario("project of a plain directory", { files: { "a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx)
  await projects(ctx)
  await projects(ctx, "?archived=true")
  await projects(ctx, "?archived=maybe")
})

scenario("project of a git repository is keyed by its root commit", { git: true, files: { "a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx)
  await projects(ctx)
})

scenario("project ID cached in the git directory", { git: true, files: { "a.txt": "a\n" } }, async (ctx) => {
  writeFileSync(path.join(ctx.project, ".git", "opencode"), "custom-project-id\n")
  await boot(ctx)
  await projects(ctx)
})

scenario("git repository without commits shares the global project", { files: { "a.txt": "a\n" } }, async (ctx) => {
  sh(ctx, "git init -q -b main")
  await boot(ctx)
  await projects(ctx)
})

scenario("linked git worktree resolves to the main checkout", { git: true, files: { "a.txt": "a\n" } }, async (ctx) => {
  sh(ctx, "git worktree add -q -b linked .worktrees/linked")
  await boot(ctx)
  await boot(ctx, path.join(ctx.project, ".worktrees/linked"))
  await projects(ctx)
  await ctx.http("GET", "/api/worktree?projectID=" + (await projects(ctx))[0]!.id)
})

scenario("mercurial marker without the hg tool", { files: { "a.txt": "a\n" } }, async (ctx) => {
  mkdirSync(path.join(ctx.project, ".hg"))
  await boot(ctx)
  await ctx.http("GET", "/api/vcs")
  await ctx.http("GET", "/api/vcs/status")
  await ctx.http("GET", "/api/vcs/branch")
  await ctx.http("GET", "/api/vcs/base")
  await ctx.http("GET", "/api/vcs/diff?mode=working")
  await projects(ctx)
})

scenario("mercurial marker with a cached project ID", { files: { "a.txt": "a\n", ".hg/opencode": "hg-project-id\n" } }, async (ctx) => {
  await boot(ctx)
  await projects(ctx)
})

scenario("markerless project adopted when a repository appears above it", { files: { "sub/a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx, path.join(ctx.project, "sub"))
  sh(ctx, "git init -q -b main && git add -A && git commit -q -m initial")
  await boot(ctx)
  await projects(ctx)
})

// Without the file watcher: moving the checkout makes the watcher reload the old location at a racy moment.
scenario("a moved checkout replaces the missing canonical directory", { files: { "a/file.txt": "x\n" }, server: { filewatcher: false } }, async (ctx) => {
  sh(ctx, "git init -q -b main && git add -A && git commit -q -m initial", path.join(ctx.project, "a"))
  await boot(ctx, path.join(ctx.project, "a"))
  await projects(ctx)
  // Same repository (same root commit) at a new path, and the old path is gone.
  renameSync(path.join(ctx.project, "a"), path.join(ctx.project, "b"))
  await boot(ctx, path.join(ctx.project, "b"))
  await projects(ctx)
})

scenario("update project metadata", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const location = await boot(ctx)
  const id = location.body.project.id
  await ctx.http("PATCH", `/api/project/${id}`, {
    name: "Demo project",
    icon: { override: "https://example.test/icon.png", color: "#336699" },
    commands: { start: "bun install" },
  })
  await ctx.http("PATCH", `/api/project/${id}`, { name: "Renamed" })
  await ctx.http("PATCH", `/api/project/${id}`, { name: "", icon: { override: "", color: "" }, commands: { start: "" } })
  await ctx.http("PATCH", `/api/project/${id}`, { canonical: path.join(ctx.project, "moved") })
  await ctx.http("PATCH", `/api/project/${id}`, { canonical: "relative" })
  await ctx.http("PATCH", "/api/project/does-not-exist", { name: "x" })
  await ctx.http("PATCH", "/api/project/does-not-exist", { canonical: "relative" })
  await ctx.http("PATCH", `/api/project/${id}`, { name: 5 })
  await projects(ctx)
})

/** Icon discovery runs in the background after a project is resolved; wait for its announcement. */
async function icon(ctx: Ctx) {
  await boot(ctx)
  await ctx.until((event) => event?.type === "project.updated" && event?.data?.icon !== undefined)
  await projects(ctx)
}

const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>'

scenario("project icon from a favicon", { files: { "public/favicon.svg": svg, "logo.png": "not really a png" } }, icon)

scenario(
  "project icon from an icon composer bundle",
  {
    files: {
      "AppIcon.icon/icon.json": JSON.stringify({
        fill: { "linear-gradient": ["srgb:1,0,0,1", "gray:0.5,1"] },
        groups: [{ layers: [{ "image-name": "missing.svg" }, { "image-name": "front.svg" }] }],
      }),
      "AppIcon.icon/Assets/front.svg": svg,
    },
  },
  icon,
)

scenario(
  "project icon from an icon composer bundle with a solid fill",
  {
    files: {
      "app/AppIcon.icon/icon.json": JSON.stringify({ fill: { solid: "display-p3:0,0.5,1,1" }, groups: [{ layers: [{ "image-name": "a.png" }] }] }),
      "app/AppIcon.icon/Assets/a.png": "png-bytes",
    },
  },
  icon,
)

scenario(
  "project icon from an asset catalog",
  {
    files: {
      "MyApp/Assets.xcassets/AppIcon.appiconset/icon-64.png": "small",
      "MyApp/Assets.xcassets/AppIcon.appiconset/icon-128.png": "preferred",
      "MyApp/Assets.xcassets/AppIcon.appiconset/Contents.json": "{}",
    },
  },
  icon,
)

scenario(
  "project icon from windows resources",
  {
    files: {
      "resources/windows/icon-16.png": "16",
      "resources/windows/icon-256.png": "256",
      "resources/windows/icon-130.png": "130",
      "resources/windows/readme.txt": "not an icon",
    },
  },
  icon,
)

scenario("location of the default and a nested directory", { files: { "sub/a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx)
  await ctx.http("GET", "/api/location", undefined, at(path.join(ctx.project, "sub")))
  await ctx.http("GET", `/api/location?location[directory]=${encodeURIComponent(path.join(ctx.project, "sub"))}`)
  await ctx.http("GET", "/api/location", undefined, at(path.join(ctx.project, "missing")))
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === path.join(ctx.project, "sub"))
})

// Without the file watcher: its reload of the written config would race the read before the explicit reload, and the
// reload itself.
scenario("reload every loaded location", { files: { "opencode.json": JSON.stringify({ username: "before" }) }, server: { filewatcher: false } }, async (ctx) => {
  await boot(ctx)
  await ctx.http("GET", "/api/config")
  ctx.write("opencode.json", JSON.stringify({ username: "after" }))
  await ctx.http("GET", "/api/config")
  const from = ctx.events.length
  await ctx.http("POST", "/api/location/reload")
  await ctx.until((event) => event?.type === "plugin.updated", from)
  await ctx.http("GET", "/api/config")
})

scenario("debug loaded locations and evict one", { files: { "sub/a.txt": "a\n" } }, async (ctx) => {
  await boot(ctx)
  await boot(ctx, path.join(ctx.project, "sub"))
  const list = await ctx.http("GET", "/api/debug/location")
  ctx.note(
    "loaded locations in this scenario",
    (list.body as Array<{ directory: string }>).filter((ref) => ref.directory.startsWith(ctx.project)).map((ref) => ref.directory).sort(),
  )
  await ctx.http("DELETE", "/api/debug/location", undefined, at(path.join(ctx.project, "sub")))
  const after = await ctx.http("GET", "/api/debug/location")
  ctx.note(
    "after evicting sub",
    (after.body as Array<{ directory: string }>).filter((ref) => ref.directory.startsWith(ctx.project)).map((ref) => ref.directory).sort(),
  )
  await ctx.http("POST", "/api/debug/heap-dump?directory=relative/dir")
})

scenario("server info", {}, async (ctx) => {
  const result = await ctx.http("GET", "/api/info")
  // The process id and machine name identify the host process, not behavior.
  if (result.body && typeof result.body === "object") {
    result.body.pid = typeof result.body.pid === "number" ? "<pid>" : result.body.pid
    result.body.hostname = typeof result.body.hostname === "string" ? "<hostname>" : result.body.hostname
  }
  await boot(ctx)
})

scenario("pairing codes", {}, async (ctx) => {
  const pair = await ctx.http("POST", "/api/pair")
  // The code is random; only its shape is behavior.
  if (pair.body && typeof pair.body.code === "string") pair.body.code = `<code of ${pair.body.code.length} chars>`
  await ctx.http("GET", "/auth/connect/not-a-real-code", undefined, { accept: "application/json" })
  await ctx.http("GET", "/auth/connect/not-a-real-code", undefined, { accept: "text/html" })
  await boot(ctx)
})

scenario("heap dump into an unusable directory", { files: { "README.md": "x\n" } }, async (ctx) => {
  const result = await ctx.http("POST", `/api/debug/heap-dump?directory=${encodeURIComponent(path.join(ctx.project, "README.md", "dumps"))}`)
  // The error reference is random per failure.
  if (result.body && typeof result.body.ref === "string") result.body.ref = "<error-ref>"
  await boot(ctx)
})

scenario("pending forms, permission requests and usage limits when idle", {}, async (ctx) => {
  await boot(ctx)
  await ctx.http("GET", "/api/form")
  await ctx.http("GET", "/api/permission/request")
  await ctx.http("GET", "/api/experimental/usage/limits")
  await ctx.http("GET", "/api/experimental/job")
})
