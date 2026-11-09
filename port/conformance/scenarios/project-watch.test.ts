// File watching while the server runs: watched files (`POST /api/experimental/fs/watch`) announce their changes as
// `filesystem.changed` on their location, and the location watcher announces changes of the repository's HEAD, after
// which the location reads its branch again (`vcs.branch.updated`).
//
// Changes reach the server through its file watcher some time after they happen, so each step waits for the events
// it causes. Files are replaced by renaming a complete file over them, as editors and git do: one change each.
// Writing a file in place is several file system events whose grouping depends on when the watcher reads them.
import { spawnSync } from "node:child_process"
import { renameSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { type Ctx, scenario } from "../lib/scenario"

/** Run a shell command (git) in the project directory to change repository state. */
function sh(ctx: Ctx, command: string) {
  const result = spawnSync("sh", ["-c", command], { cwd: ctx.project, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr}`)
}

/** Replace a project file in one step: write a temporary file next to it, then rename it over the file. */
function replace(ctx: Ctx, file: string, content: string) {
  const target = path.join(ctx.project, file)
  writeFileSync(`${target}.tmp`, content)
  renameSync(`${target}.tmp`, target)
}

const changed = (ctx: Ctx, from: number, file: string, event: string) =>
  ctx.until(
    (e) => e?.type === "filesystem.changed" && e.data?.file === path.join(ctx.project, file) && e.data?.event === event,
    from,
  )

/** Long enough for a change to have reached the server, had it been watched. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 500))

scenario(
  "a watched file announces its changes and its removal",
  { files: { "notes.txt": "one\n", "other.txt": "x\n" } },
  async (ctx) => {
    await ctx.http("POST", "/api/experimental/fs/watch", { path: "notes.txt" })
    await settle()
    let from = ctx.events.length
    replace(ctx, "notes.txt", "two\n")
    await changed(ctx, from, "notes.txt", "change")
    // A sibling of the watched file is not watched.
    replace(ctx, "other.txt", "y\n")
    await settle()
    from = ctx.events.length
    rmSync(path.join(ctx.project, "notes.txt"))
    await changed(ctx, from, "notes.txt", "unlink")
    // The watch outlives the file: recreating it is a change.
    from = ctx.events.length
    replace(ctx, "notes.txt", "three\n")
    await changed(ctx, from, "notes.txt", "change")
    // Watching it again only renews the watch: still one event per change.
    await ctx.http("POST", "/api/experimental/fs/watch", { path: "notes.txt" })
    from = ctx.events.length
    replace(ctx, "notes.txt", "four\n")
    await changed(ctx, from, "notes.txt", "change")
    await settle()
    // Unwatched, it changes silently.
    await ctx.http("DELETE", "/api/experimental/fs/watch", { path: "notes.txt" })
    await settle()
    replace(ctx, "notes.txt", "five\n")
    await settle()
  },
)

scenario(
  "watches of a nested location announce on that location",
  { files: { "pkg/a.txt": "a\n", "pkg/b.txt": "b\n" } },
  async (ctx) => {
    const nested = { "x-opencode-directory": encodeURIComponent(path.join(ctx.project, "pkg")) }
    await ctx.http("POST", "/api/experimental/fs/watch", { path: "a.txt" }, nested)
    await ctx.http("POST", "/api/experimental/fs/watch", { path: "pkg/b.txt" })
    await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === path.join(ctx.project, "pkg"))
    await settle()
    let from = ctx.events.length
    replace(ctx, "pkg/a.txt", "a2\n")
    await changed(ctx, from, "pkg/a.txt", "change")
    from = ctx.events.length
    replace(ctx, "pkg/b.txt", "b2\n")
    await changed(ctx, from, "pkg/b.txt", "change")
    await settle()
  },
)

scenario("watching needs the file watcher", { files: { "notes.txt": "one\n" }, server: { filewatcher: false } }, async (ctx) => {
  await ctx.http("POST", "/api/experimental/fs/watch", { path: "notes.txt" })
  await ctx.http("DELETE", "/api/experimental/fs/watch", { path: "notes.txt" })
})

const branch = (ctx: Ctx, from: number, name?: string) =>
  ctx.until((event) => event?.type === "vcs.branch.updated" && event.data?.branch === name, from)

scenario("branch switches announce the new branch", { git: true, files: { "a.txt": "a\n" } }, async (ctx) => {
  await ctx.http("GET", "/api/vcs")
  await settle()
  let from = ctx.events.length
  sh(ctx, "git checkout -q -b feature")
  await branch(ctx, from, "feature")
  await ctx.http("GET", "/api/vcs")
  // A commit moves the branch, not HEAD: nothing to announce.
  sh(ctx, "git commit -q --allow-empty -m second")
  await settle()
  from = ctx.events.length
  sh(ctx, "git checkout -q --detach HEAD~1")
  await branch(ctx, from, undefined)
  await ctx.http("GET", "/api/vcs")
  from = ctx.events.length
  sh(ctx, "git checkout -q main")
  await branch(ctx, from, "main")
  await ctx.http("GET", "/api/vcs")
  await settle()
})

// Only the nested location is booted: two locations watching one HEAD would announce in either order.
scenario(
  "a nested location watches the repository's HEAD",
  { boot: false, git: true, files: { "src/a.txt": "a\n" } },
  async (ctx) => {
    const nested = { "x-opencode-directory": encodeURIComponent(path.join(ctx.project, "src")) }
    await ctx.http("GET", "/api/vcs", undefined, nested)
    await ctx.until((event) => event?.type === "plugin.updated")
    await settle()
    const from = ctx.events.length
    sh(ctx, "git checkout -q -b topic")
    await branch(ctx, from, "topic")
    await ctx.http("GET", "/api/vcs", undefined, nested)
    await settle()
  },
)

scenario(
  "watcher.ignore turns the HEAD watch off",
  { git: true, files: { "a.txt": "a\n" }, config: { watcher: { ignore: [".git"] } } },
  async (ctx) => {
    await ctx.http("GET", "/api/vcs")
    await settle()
    sh(ctx, "git checkout -q -b quiet")
    await settle()
    // Nothing re-read the branch.
    await ctx.http("GET", "/api/vcs")
  },
)

scenario("a markerless location watches no HEAD", { files: { "a.txt": "a\n" } }, async (ctx) => {
  await ctx.http("GET", "/api/vcs")
  await settle()
  sh(ctx, "git init -q -b trunk")
  await settle()
  await ctx.http("GET", "/api/vcs")
})
