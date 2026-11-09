// The project archive sweep: when a server starts (and every hour after), each project untouched for an hour whose
// canonical directory is gone moves to another main checkout it knows that still exists, or is archived when none
// survives (archived projects are listed only with `archived=true`; resolving one again unarchives it).
//
// An hour of inactivity cannot pass inside a scenario, so the scenario ages the projects in the on-disk database
// (`<data>/opencode/opencode-<channel>.db`, the public storage format) while the service is stopped, removes their
// directories, and starts the service again. Requests go through `opencode api`, which reaches the managed service.
import { Database } from "bun:sqlite"
import { spawnSync } from "node:child_process"
import { mkdirSync, readdirSync, rmSync } from "node:fs"
import path from "node:path"
import { cliScenario } from "../lib/cli"

function git(cwd: string, ...args: string[]) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`)
}

/** A checkout whose project is identified by its origin remote: clones of one remote share a project. */
function checkout(directory: string) {
  mkdirSync(directory, { recursive: true })
  git(directory, "init", "-q", "-b", "main")
  git(directory, "remote", "add", "origin", "https://example.com/conformance/shared.git")
}

/** The location of a directory (the managed service's own working directory is its default). */
const locate = (directory: string) => `/api/location?location[directory]=${encodeURIComponent(directory)}`

/** Moves every project's activity two hours into the past. */
function age(sandbox: string) {
  const data = path.join(sandbox, "data", "opencode")
  const file = readdirSync(data).find((name) => /^opencode.*\.db$/.test(name))
  if (!file) throw new Error(`no database in ${data}`)
  const db = new Database(path.join(data, file))
  db.run("UPDATE project SET time_updated = time_updated - 7200000, time_active = time_active - 7200000")
  db.close()
}

cliScenario("projects whose directories are gone are archived or moved", {}, async (ctx) => {
  const at = (name: string) => path.join(ctx.sandbox, "places", name)
  for (const name of ["gone", "kept"]) mkdirSync(at(name), { recursive: true })
  checkout(at("first-clone"))
  checkout(at("second-clone"))
  for (const name of ["gone", "kept", "first-clone", "second-clone"])
    await ctx.cli(["api", "GET", locate(at(name))])
  await ctx.cli(["service", "stop"])
  age(ctx.sandbox)
  rmSync(at("gone"), { recursive: true })
  rmSync(at("first-clone"), { recursive: true })
  // The first request starts the service, whose sweep runs as it starts; the listings come after it.
  await ctx.cli(["api", "GET", "/api/session"])
  await ctx.cli(["api", "GET", "/api/project"])
  await ctx.cli(["api", "GET", "/api/project?archived=true"])
  // Resolving an archived project again unarchives it.
  mkdirSync(at("gone"), { recursive: true })
  await ctx.cli(["api", "GET", locate(at("gone"))])
  await ctx.cli(["api", "GET", "/api/project"])
})

cliScenario("recently active projects are not swept", {}, async (ctx) => {
  const gone = path.join(ctx.sandbox, "places", "recent")
  mkdirSync(gone, { recursive: true })
  await ctx.cli(["api", "GET", locate(gone)])
  await ctx.cli(["service", "stop"])
  rmSync(gone, { recursive: true })
  await ctx.cli(["api", "GET", "/api/session"])
  await ctx.cli(["api", "GET", "/api/project?archived=true"])
})
