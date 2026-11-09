// Git references (config `references`) are checked out into the repository cache (`<data>/opencode/repos/<host>/<path>`,
// `@<branch>` for a pinned branch) in the background when a location boots, and refreshed (fetched, then hard-reset to
// the tracked branch) at most once a day: the last attempt is kept in the database (`kv`, `repository-cache:<path>`).
//
// GitHub references clone from `OPENCODE_REPO_CLONE_GITHUB_BASE_URL` when it is set: here a directory of bare
// repositories in the sandbox, so nothing reaches the network. A day cannot pass inside a scenario: the scenario
// ages the recorded attempt in the on-disk database while the server is stopped, as it would read after a day.
import { Database } from "bun:sqlite"
import { spawnSync } from "node:child_process"
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"
import { cliScenario, type CliCtx } from "../lib/cli"

const LISTENING = /^server listening on (http:\/\/\S+)$/
const auth = { authorization: `Basic ${Buffer.from("opencode:conformance-secret").toString("base64")}` }

function git(cwd: string, ...args: string[]) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" })
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`)
}

/** A bare repository `<remotes>/conformance/notes.git` with `main` and `stable`, and the work tree that feeds it. */
function remote(sandbox: string) {
  const work = path.join(sandbox, "work", "notes")
  mkdirSync(work, { recursive: true })
  git(work, "init", "-q", "-b", "main")
  writeFileSync(path.join(work, "README.md"), "one\n")
  git(work, "add", "-A")
  git(work, "commit", "-q", "-m", "one")
  git(work, "branch", "stable")
  const bare = path.join(sandbox, "remotes", "conformance", "notes.git")
  mkdirSync(path.dirname(bare), { recursive: true })
  git(path.dirname(bare), "clone", "-q", "--bare", work, bare)
  git(work, "remote", "add", "origin", bare)
  return {
    remotes: path.join(sandbox, "remotes"),
    commit(file: string, content: string, branch = "main") {
      git(work, "checkout", "-q", branch)
      writeFileSync(path.join(work, file), content)
      git(work, "commit", "-q", "-am", content.trim())
      git(work, "push", "-q", "origin", branch)
    },
  }
}

const checkout = (sandbox: string, name: string) => path.join(sandbox, "data", "opencode", "repos", "github.com", "conformance", name)

/** Wait until a checked-out file has the content (the checkout runs in the background). */
async function until(file: string, content: string) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    if (existsSync(file) && readFileSync(file, "utf8") === content) return
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  throw new Error(`${file} never read ${JSON.stringify(content)}`)
}

/** Long enough for a refresh that should not happen to have happened. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 1500))

async function serve(ctx: CliCtx, env: Record<string, string>) {
  const server = ctx.start(["serve", "--port", "0"], { env })
  const url = LISTENING.exec(await server.line(LISTENING))![1]
  // Booting the project location checks its references out.
  await ctx.fetch("GET", `${url}/api/reference?directory=${encodeURIComponent(ctx.project)}`, undefined, auth)
  return server
}

/** Moves every recorded checkout attempt two days into the past. */
function age(sandbox: string) {
  const data = path.join(sandbox, "data", "opencode")
  const file = readdirSync(data).find((name) => /^opencode.*\.db$/.test(name))
  if (!file) throw new Error(`no database in ${data}`)
  const db = new Database(path.join(data, file))
  db.run(
    "UPDATE kv SET value = json_set(value, '$.attemptedAt', json_extract(value, '$.attemptedAt') - 172800000) WHERE key LIKE 'repository-cache:%'",
  )
  db.close()
}

cliScenario(
  "git references are checked out and refreshed daily",
  {
    config: {
      references: {
        notes: { repository: "github:conformance/notes", description: "Team notes." },
        pinned: { repository: "conformance/notes", branch: "stable" },
      },
    },
  },
  async (ctx) => {
    const notes = remote(ctx.sandbox)
    const env = { OPENCODE_PASSWORD: "conformance-secret", OPENCODE_REPO_CLONE_GITHUB_BASE_URL: `file://${notes.remotes}/` }
    let server = await serve(ctx, env)
    await until(path.join(checkout(ctx.sandbox, "notes"), "README.md"), "one\n")
    await until(path.join(checkout(ctx.sandbox, "notes@stable"), "README.md"), "one\n")
    await settle()
    await server.stop()
    // The remote moves on; within a day of the last attempt a new server leaves the checkouts alone.
    notes.commit("README.md", "two\n")
    notes.commit("README.md", "stable two\n", "stable")
    server = await serve(ctx, env)
    await settle()
    ctx.file("data/opencode/repos/github.com/conformance/notes/README.md")
    ctx.file("data/opencode/repos/github.com/conformance/notes@stable/README.md")
    await server.stop()
    // A day later, the next boot fetches and resets each checkout to its branch.
    age(ctx.sandbox)
    server = await serve(ctx, env)
    await until(path.join(checkout(ctx.sandbox, "notes"), "README.md"), "two\n")
    await until(path.join(checkout(ctx.sandbox, "notes@stable"), "README.md"), "stable two\n")
    await settle()
    ctx.file("data/opencode/repos/github.com/conformance/notes/README.md")
    ctx.file("data/opencode/repos/github.com/conformance/notes@stable/README.md")
    await server.stop()
  },
)

cliScenario(
  "a cache directory of another repository is replaced",
  { config: { references: { notes: "github:conformance/notes" } } },
  async (ctx) => {
    const notes = remote(ctx.sandbox)
    // Something else already sits where the checkout goes.
    const stale = checkout(ctx.sandbox, "notes")
    mkdirSync(stale, { recursive: true })
    writeFileSync(path.join(stale, "README.md"), "stale\n")
    const env = { OPENCODE_PASSWORD: "conformance-secret", OPENCODE_REPO_CLONE_GITHUB_BASE_URL: `file://${notes.remotes}/` }
    const server = await serve(ctx, env)
    await until(path.join(stale, "README.md"), "one\n")
    await settle()
    ctx.file("data/opencode/repos/github.com/conformance/notes/README.md")
    await server.stop()
  },
)
