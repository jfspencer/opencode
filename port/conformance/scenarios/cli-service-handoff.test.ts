// The persistent-terminal handoff between background services (packages/client/src/pty-handoff.ts). A client that
// replaces a ready service publishes the daemon's handoff ticket beside the registration
// (`<registration>.pty-handoff`, with the service it came from); the contenders it starts next get the ticket
// (OPENCODE_PTY_HANDOFF) while the sidecar still describes the registered service, and the winner adopts the daemon
// with its terminals. A client that finds another service running clears the sidecar.
//
// No CLI command replaces a ready service by itself (the TUI does, on a version mismatch or its restart command), so
// the scenario stands in for that client's first half: it asks the running service for a ticket, publishes the
// sidecar, and kills the service. Any command then starts the replacement. Everything stays in the sandbox: its
// registration, its pinned port, its daemon.
import { readFileSync, writeFileSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { say } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}
const REGISTRATION = "state/opencode/service-conformance.json"
const SIDECAR = `${REGISTRATION}.pty-handoff`
const READY = 'printf "ready\\n"; read a; printf "got %s\\n" "$a"; read b'

type Registration = { readonly id?: string; readonly pid: number; readonly url: string; readonly password?: string }

/**
 * The environment of the commands that start a service: its daemon's socket goes in the run's sandbox (lib/preload.ts),
 * not the user's /tmp/opencode-pty-<uid>; recorded as `<pty-sockets>`. The daemon itself is the one each
 * implementation ships (the reference's `@opencode/pty`, the Rust build's embedded copy), as for users.
 */
function sockets(ctx: CliCtx) {
  const directory = process.env.OPENCODE_PTY_SOCKET_DIR!
  if (!scrubbed.has(ctx)) {
    scrubbed.add(ctx)
    ctx.scrub((text) => text.split(directory).join("<pty-sockets>"))
  }
  return { OPENCODE_PTY_SOCKET_DIR: directory }
}
const scrubbed = new WeakSet<CliCtx>()

const registration = (ctx: CliCtx): Registration => JSON.parse(readFileSync(path.join(ctx.sandbox, REGISTRATION), "utf8"))

/** Requests to the running service, as its own clients send them (its password, the project's location). */
function api(ctx: CliCtx) {
  const service = registration(ctx)
  const headers = {
    authorization: `Basic ${btoa(`opencode:${service.password}`)}`,
    "x-opencode-directory": encodeURIComponent(ctx.project),
  }
  return (method: string, route: string, body?: unknown) => ctx.fetch(method, `${service.url}${route}`, body, headers)
}

function sessionOf(stdout: string) {
  const line = stdout.split("\n").find((item) => item.includes('"sessionID"'))
  if (!line) throw new Error(`no session in output: ${stdout}`)
  return JSON.parse(line).sessionID as string
}

/** Starts the service with a session that has one running terminal; returns the session. */
async function terminalSession(ctx: CliCtx) {
  // `run` uses the service only when one is running.
  await ctx.cli(["service", "start"], { env: sockets(ctx) })
  const session = sessionOf((await ctx.cli(["run", "--title", "Terminals", "--format", "json", "Hello"])).stdout)
  const request = api(ctx)
  await request("POST", `/api/experimental/session/${session}/terminal`, {
    command: "/usr/bin/env",
    args: ["sh", "-c", READY],
    cwd: ctx.project,
    title: "Survivor",
    env: { GREETING: "hi" },
  })
  return session
}

/** Kills the registered service at once: its registration stays behind, as after a crash. */
async function kill(service: Registration) {
  process.kill(service.pid, "SIGKILL")
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    try {
      process.kill(service.pid, 0)
    } catch {
      return
    }
    await Bun.sleep(20)
  }
  throw new Error(`service ${service.pid} did not exit`)
}

const terminals = (ctx: CliCtx, session: string) => api(ctx)("GET", `/api/experimental/session/${session}/terminal`)

cliScenario(
  "a replacement service adopts the persistent terminals of a published handoff",
  { config: quiet, turns: [say("Hello.")] },
  async (ctx) => {
    const session = await terminalSession(ctx)
    const service = registration(ctx)
    const ticket = (await api(ctx)("POST", "/api/experimental/persistent-pty/handoff")).body.handoff
    const sidecar = { source: { id: service.id, pid: service.pid, url: service.url }, handoff: ticket, expiresAt: ticket.expiresAt }
    writeFileSync(path.join(ctx.sandbox, SIDECAR), JSON.stringify(sidecar) + "\n")
    await kill(service)
    // The next command starts a replacement with the ticket, which adopts the daemon and its terminal; finding the
    // replacement running, the client clears the sidecar.
    await ctx.cli(["service", "status"])
    await ctx.cli(["session", "list", "--format", "json"], { env: sockets(ctx) })
    ctx.file(SIDECAR)
    await terminals(ctx, session)
  },
)

cliScenario(
  "a handoff published by another service is not adopted",
  { config: quiet, turns: [say("Hello.")] },
  async (ctx) => {
    const session = await terminalSession(ctx)
    const service = registration(ctx)
    const ticket = (await api(ctx)("POST", "/api/experimental/persistent-pty/handoff")).body.handoff
    // The sidecar names a service other than the registered one.
    const sidecar = { source: { id: service.id, pid: service.pid + 1, url: service.url }, handoff: ticket, expiresAt: ticket.expiresAt }
    writeFileSync(path.join(ctx.sandbox, SIDECAR), JSON.stringify(sidecar) + "\n")
    await kill(service)
    await ctx.cli(["session", "list", "--format", "json"], { env: sockets(ctx) })
    ctx.file(SIDECAR)
    await terminals(ctx, session)
  },
)

cliScenario(
  "an expired or empty handoff is not adopted",
  { config: quiet, turns: [say("Hello.")] },
  async (ctx) => {
    const session = await terminalSession(ctx)
    const service = registration(ctx)
    const ticket = (await api(ctx)("POST", "/api/experimental/persistent-pty/handoff")).body.handoff
    const source = { id: service.id, pid: service.pid, url: service.url }
    writeFileSync(path.join(ctx.sandbox, SIDECAR), JSON.stringify({ source, handoff: ticket, expiresAt: Date.now() - 1_000 }) + "\n")
    await kill(service)
    await ctx.cli(["session", "list", "--format", "json"], { env: sockets(ctx) })
    ctx.file(SIDECAR)
    await terminals(ctx, session)
    // A sidecar of the running service (here one without a ticket: nothing to hand off) is kept by a client that
    // reuses that service, and dropped when the service is stopped.
    const next = registration(ctx)
    writeFileSync(
      path.join(ctx.sandbox, SIDECAR),
      JSON.stringify({ source: { id: next.id, pid: next.pid, url: next.url }, handoff: null, expiresAt: Date.now() + 30_000 }) + "\n",
    )
    await ctx.cli(["session", "list", "--format", "json"], { env: sockets(ctx) })
    ctx.file(SIDECAR)
    await ctx.cli(["service", "stop"])
    ctx.file(SIDECAR)
  },
)
