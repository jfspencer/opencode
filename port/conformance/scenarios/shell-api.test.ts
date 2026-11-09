// Shell command routes: the location's shell commands (list/get/output/remove), commands a user runs in a session,
// and how the configured shell is resolved for the shell tool.
//
// Shell.Info carries the process ID, which differs on every run, so routes that return it for a live command
// (POST /api/shell, GET /api/shell/:id, GET /api/shell while one runs, shell.started/ended events) are only
// exercised where the response does not include it.
import { HIDDEN_TOOLS } from "../lib/model"
import { call, say } from "../lib/openrouter"
import { scenario, type Ctx } from "../lib/scenario"
import { chmodSync, existsSync, mkdirSync } from "node:fs"
import path from "node:path"

type Effect = "allow" | "deny" | "ask"
const rule = (action: string, resource: string, effect: Effect) => ({ action, resource, effect })
const only = [...HIDDEN_TOOLS.map((action) => rule(action, "*", "deny")), rule("*", "*", "deny")]
const agents = { build: { system: "Conformance agent." } }
const allowShell = { agents, permissions: [...only, rule("shell", "*", "allow")] }

scenario("shell routes for unknown commands", {}, async (ctx) => {
  // Wait for the location's plugins, so the startup events are always complete.
  await ctx.http("GET", "/api/provider")
  await ctx.http("GET", "/api/shell")
  await ctx.http("GET", "/api/shell/sh_conformance_missing")
  await ctx.http("GET", "/api/shell/sh_conformance_missing/output")
  await ctx.http("DELETE", "/api/shell/sh_conformance_missing")
  await ctx.http("GET", "/api/shell/not-a-shell-id")
  await ctx.http("GET", "/api/shell/sh_conformance_missing/output?cursor=-1")
  await ctx.http("POST", "/api/shell", {})
  // A working directory that does not exist is rejected.
  await ctx.http("POST", "/api/shell", { command: "true", cwd: "/conformance/missing/directory" })
  // Commands that cannot start: one too long for the system to pass to the shell (Linux limits one argument to
  // 128 KiB), one with a NUL character, and one in a directory the server may not enter (unless it runs as root).
  await ctx.http("POST", "/api/shell", { command: `echo ${"x".repeat(200_000)}` })
  await ctx.http("POST", "/api/shell", { command: "echo a\u0000b" })
  const locked = path.join(ctx.project, "locked")
  mkdirSync(locked)
  chmodSync(locked, 0o000)
  await ctx.http("POST", "/api/shell", { command: "true", cwd: locked })
  chmodSync(locked, 0o755)
  await ctx.http("GET", "/api/shell")
})

// The shells offered for terminals and agents come from /etc/shells, so this recording is specific to the host.
scenario("list available shells", {}, async (ctx) => {
  await ctx.http("GET", "/api/provider")
  await ctx.http("GET", "/api/config/shell")
})

const progress = (ctx: Ctx, id: string) =>
  ctx.until((event: any) => event?.type === "session.tool.progress" && event.data?.sessionID === id)

scenario(
  "page through the output of a finished command",
  {
    config: allowShell,
    // One stream: stdout and stderr are read concurrently, so the byte offsets below would depend on timing.
    turns: [call("shell", { command: "printf 'one\\ntwo\\nthree\\n'; printf 'émoji ✓\\n'" }), say("Done.")],
  },
  async (ctx) => {
    const id = await ctx.session({ title: "output" })
    await ctx.prompt(id, "Print some lines")
    const shell = (await progress(ctx, id)).data.metadata.shellID
    // Finished commands stay readable but are no longer listed.
    await ctx.http("GET", "/api/shell")
    await ctx.http("GET", `/api/shell/${shell}/output`)
    await ctx.http("GET", `/api/shell/${shell}/output?limit=4`)
    await ctx.http("GET", `/api/shell/${shell}/output?cursor=4&limit=4`)
    await ctx.http("GET", `/api/shell/${shell}/output?cursor=8`)
    // Cursors count bytes, so a page can end inside a multi-byte character.
    await ctx.http("GET", `/api/shell/${shell}/output?cursor=14&limit=2`)
    // A zero limit reads nothing and leaves the cursor where it was.
    await ctx.http("GET", `/api/shell/${shell}/output?cursor=4&limit=0`)
    await ctx.http("GET", `/api/shell/${shell}/output?cursor=1000`)
    await ctx.http("DELETE", `/api/shell/${shell}`)
    await ctx.http("GET", `/api/shell/${shell}/output`)
    await ctx.http("DELETE", `/api/shell/${shell}`)
  },
)

// Events carry the shell's process ID, so they are not recorded; the session's messages show the outcome.
scenario(
  "run commands in a session as the user",
  { files: { "notes.txt": "first\nsecond\n" }, events: false },
  async (ctx) => {
    const id = await ctx.session({ title: "user shell" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "cat notes.txt" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "echo partial; exit 3" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "true" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "kill -KILL $$" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "echo 'unterminated" })
    await ctx.http("POST", `/api/session/${id}/shell`, { id: "msg_conformance_user_shell", command: "echo with id" })
    await ctx.http("POST", `/api/session/${id}/shell`, {})
    await ctx.http("POST", "/api/session/ses_conformance_missing/shell", { command: "true" })
    // Commands that cannot start are rejected; the session is told they failed to start.
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "echo a\u0000b" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: `echo ${"y".repeat(200_000)}` })
    await ctx.http("GET", `/api/session/${id}/message`)
    await ctx.http("GET", `/api/session/${id}/inbox`)
  },
)

scenario(
  "the model sees commands the user ran",
  {
    events: false,
    config: allowShell,
    turns: [say("I see what you ran.")],
    files: { "a.txt": "a\n" },
  },
  async (ctx) => {
    const id = await ctx.session({ title: "user shell context" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "ls" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "echo partial; exit 3" })
    await ctx.http("POST", `/api/session/${id}/shell`, { command: "kill -KILL $$" })
    // A command removed while it runs is reported as cancelled. Its shell ID comes from the session's messages once
    // it has started (the command marks that on disk; the margin covers recording the start).
    const running = ctx.http("POST", `/api/session/${id}/shell`, { command: "touch started; until [ -f never ]; do sleep 0.05; done" })
    while (!existsSync(path.join(ctx.project, "started"))) await new Promise((resolve) => setTimeout(resolve, 10))
    await new Promise((resolve) => setTimeout(resolve, 500))
    const messages = await ctx.http("GET", `/api/session/${id}/message`)
    const shell = messages.body.data.find((message: any) => message.type === "shell" && message.status === "running").shellID
    await ctx.http("DELETE", `/api/shell/${shell}`)
    await running
    await ctx.http("POST", `/api/session/${id}/prompt`, { text: "What did I just run?" })
    await ctx.wait(id)
  },
)

// The configured shell decides how commands are scanned and run. Incompatible or missing shells fall back to the
// default one.
for (const [name, shell] of [
  ["sh", "sh"],
  ["an absolute path", "/bin/sh"],
  ["an incompatible shell", "fish"],
  ["a missing shell", "/conformance/missing/zsh"],
] as const)
  scenario(
    `configured shell: ${name}`,
    {
      config: { ...allowShell, shell },
      turns: [call("shell", { command: "echo $0; echo [[ -n x ]]" }), say("Done.")],
    },
    async (ctx) => {
      const id = await ctx.session({ title: name })
      await ctx.prompt(id, "Which shell runs commands?")
    },
  )
