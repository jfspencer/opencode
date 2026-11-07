// `opencode session list/delete/export/import`: managing sessions from the command line.
import { rmSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { say } from "../lib/openrouter"
import { cliScenario } from "../lib/cli"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}

/** The session ID a `run --format json` printed. */
function sessionOf(stdout: string) {
  const line = stdout.split("\n").find((item) => item.includes('"sessionID"'))
  if (!line) throw new Error(`no session in output: ${stdout}`)
  return JSON.parse(line).sessionID as string
}

cliScenario("session list with no sessions", {}, async (ctx) => {
  await ctx.cli(["session", "list"])
  await ctx.cli(["session", "list", "--format", "json"])
})

cliScenario(
  "session list formats and limits",
  { config: quiet, turns: [say("One."), say("Two."), say("Three.")] },
  async (ctx) => {
    await ctx.cli(["run", "--title", "First session", "One"])
    await ctx.cli(["run", "--title", "Second session", "Two"])
    await ctx.cli(["run", "--title", "Third\tsession\nwith whitespace", "Three"])
    await ctx.cli(["session", "list"])
    await ctx.cli(["session", "list", "--format", "json"])
    await ctx.cli(["session", "list", "-n", "2"])
    await ctx.cli(["session", "list", "--max-count", "1", "--format", "json"])
  },
)

cliScenario(
  "session list is scoped to the current project",
  { git: true, files: { "sub/keep.txt": "x\n" }, config: quiet, turns: [say("Here.")] },
  async (ctx) => {
    await ctx.cli(["run", "--title", "Project session", "Hello"])
    await ctx.cli(["session", "list", "--format", "json"], { cwd: `${ctx.project}/sub` })
    await ctx.cli(["session", "list", "--format", "json"], { cwd: ctx.sandbox })
  },
)

cliScenario(
  "session delete",
  { config: quiet, turns: [say("Kept."), say("Deleted.")] },
  async (ctx) => {
    await ctx.cli(["run", "--title", "Keep me", "One"])
    const doomed = sessionOf((await ctx.cli(["run", "--title", "Delete me", "--format", "json", "Two"])).stdout)
    await ctx.cli(["session", "delete", doomed])
    await ctx.cli(["session", "list", "--format", "json"])
    await ctx.cli(["session", "delete", doomed])
    await ctx.cli(["session", "delete", "ses_doesNotExist0000000000001"])
    await ctx.cli(["session", "delete", "not-a-session-id"])
  },
)

cliScenario(
  "session export",
  { files: { "secret.txt": "token=abc123\n" }, config: quiet, turns: [say("The token is abc123.")] },
  async (ctx) => {
    const id = sessionOf((await ctx.cli(["run", "--title", "Export me", "--format", "json", "-f", "secret.txt", "What is the token?"])).stdout)
    await ctx.cli(["session", "export", id])
    await ctx.cli(["session", "export", id, "--sanitize"])
    await ctx.cli(["session", "export"])
    await ctx.cli(["session", "export", "ses_doesNotExist0000000000001"])
  },
)

cliScenario(
  "session import",
  { config: quiet, turns: [say("Original reply.")] },
  async (ctx) => {
    const id = sessionOf((await ctx.cli(["run", "--title", "Round trip", "--format", "json", "Hello"])).stdout)
    const exported = await ctx.cli(["session", "export", id])
    ctx.write("export.json", exported.stdout)
    // Importing a session that still exists is a conflict.
    await ctx.cli(["session", "import", "export.json"])
    await ctx.cli(["session", "delete", id])
    await ctx.cli(["session", "import", "export.json"])
    await ctx.cli(["session", "list", "--format", "json"])
    await ctx.cli(["session", "export", id])
    // The export file holds raw timestamps; it is an input, not a result.
    rmSync(path.join(ctx.project, "export.json"))
  },
)

cliScenario(
  "session import into another directory",
  { files: { "other/README.md": "# Other\n" }, config: quiet, turns: [say("Reply.")] },
  async (ctx) => {
    const id = sessionOf((await ctx.cli(["run", "--title", "Moved", "--format", "json", "Hello"])).stdout)
    ctx.write("export.json", (await ctx.cli(["session", "export", id])).stdout)
    await ctx.cli(["session", "delete", id])
    await ctx.cli(["session", "import", "export.json", "--directory", "other"])
    await ctx.cli(["session", "list", "--format", "json"], { cwd: `${ctx.project}/other` })
    rmSync(path.join(ctx.project, "export.json"))
  },
)

cliScenario("session import from a URL", { config: quiet, turns: [say("Served reply.")] }, async (ctx) => {
  const id = sessionOf((await ctx.cli(["run", "--title", "Over the wire", "--format", "json", "Hello"])).stdout)
  const exported = (await ctx.cli(["session", "export", id])).stdout
  await ctx.cli(["session", "delete", id])
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: await ctx.port(),
    fetch: (request) => (new URL(request.url).pathname === "/export.json" ? new Response(exported) : new Response("gone", { status: 404 })),
  })
  try {
    await ctx.cli(["session", "import", `http://127.0.0.1:${server.port}/export.json`])
    await ctx.cli(["session", "import", `http://127.0.0.1:${server.port}/missing.json`])
  } finally {
    await server.stop(true)
  }
  await ctx.cli(["session", "list", "--format", "json"])
})

cliScenario("session import errors", { files: { "broken.json": "{ not json", "empty.json": "{}", "list.json": "[]" } }, async (ctx) => {
  await ctx.cli(["session", "import", "missing.json"])
  await ctx.cli(["session", "import", "broken.json"])
  await ctx.cli(["session", "import", "list.json"])
  await ctx.cli(["session", "import", "empty.json"])
})
