// What the CLI does differently on a terminal: help, version and errors in color, the session table through the
// pager, the session picker of `session export`, and times in the local time zone.
import { chmodSync, mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { HIDDEN_TOOLS } from "../lib/model"
import { say } from "../lib/openrouter"
import { cliScenario, type CliCtx } from "../lib/cli"

const quiet = {
  permissions: [...HIDDEN_TOOLS.map((action) => ({ action, resource: "*", effect: "deny" })), { action: "*", resource: "*", effect: "deny" }],
}

/** The session ID a `run --format json` printed. */
function sessionOf(stdout: string) {
  const line = stdout.split("\n").find((item) => item.includes('"sessionID"'))
  if (!line) throw new Error(`no session in output: ${stdout}`)
  return JSON.parse(line).sessionID as string
}

/**
 * A PATH whose first directory holds `programs` (name to shell script), recorded as `<sandbox>/bin:<PATH>`;
 * `inherit: false` gives a PATH of that directory alone.
 */
function stubPath(ctx: CliCtx, programs: Record<string, string>, inherit = true) {
  const bin = path.join(ctx.sandbox, inherit ? "bin" : "only-bin")
  mkdirSync(bin, { recursive: true })
  for (const [name, script] of Object.entries(programs)) {
    writeFileSync(path.join(bin, name), script)
    chmodSync(path.join(bin, name), 0o755)
  }
  const inherited = process.env.PATH ?? ""
  ctx.scrub((text) => (inherited ? text.split(inherited).join("<PATH>") : text))
  return inherit ? `${bin}:${inherited}` : bin
}

// A pager that shows how it was started, then what it was given.
const LESS = '#!/bin/sh\necho "less $*"\ncat\n'

/** Replaces the hint's last eight characters of each session ID with which session they belong to. */
function tails(ctx: CliCtx, ids: readonly string[]) {
  ctx.scrub((text) => ids.reduce((out, id, index) => out.split(` - ${id.slice(-8)})`).join(` - <tail of session ${index + 1}>)`), text))
}

cliScenario("help, version and errors in color on a terminal", {}, async (ctx) => {
  await ctx.terminal(["--help"], [], { styles: true })
  await ctx.terminal(["session", "list", "--help"], [], { styles: true })
  await ctx.terminal(["service", "--help"], [], { styles: true })
  await ctx.terminal(["--version"], [], { styles: true })
  await ctx.terminal(["session", "list", "--format", "xml"], [], { styles: true })
  await ctx.terminal(["run", "--frobnicate", "--model"], [], { styles: true })
  // Colors stay off with NO_COLOR or a dumb terminal, and FORCE_COLOR turns them on without a terminal.
  await ctx.terminal(["--version"], [], { styles: true, env: { NO_COLOR: "1" } })
  await ctx.terminal(["--version"], [], { styles: true, env: { TERM: "dumb" } })
  await ctx.terminal(["--version"], [], { styles: true, env: { NO_COLOR: "" } })
  await ctx.cli(["--version"], { env: { FORCE_COLOR: "1" } })
  await ctx.cli(["--version"], { env: { FORCE_COLOR: "0" } })
  await ctx.cli(["session", "list", "--help"], { env: { FORCE_COLOR: "true" } })
})

cliScenario(
  "session list on a terminal goes through the pager",
  { config: quiet, turns: [say("One."), say("Two.")] },
  async (ctx) => {
    await ctx.cli(["run", "--title", "First session", "One"])
    await ctx.cli(["run", "--title", "Second session", "Two"])
    const pager = stubPath(ctx, { less: LESS })
    await ctx.terminal(["session", "list"], [], { env: { PATH: pager } })
    // A limit, JSON, or output that is not a terminal skip the pager.
    await ctx.terminal(["session", "list", "-n", "5"], [], { env: { PATH: pager } })
    await ctx.terminal(["session", "list", "--format", "json"], [], { env: { PATH: pager } })
    await ctx.cli(["session", "list"], { env: { PATH: pager } })
    // Without a pager to start, the table is written directly.
    await ctx.terminal(["session", "list"], [], { env: { PATH: stubPath(ctx, {}, false) } })
  },
)

cliScenario("session list on a terminal with no sessions", {}, async (ctx) => {
  await ctx.terminal(["session", "list"], [], { env: { PATH: stubPath(ctx, { less: LESS }) } })
})

cliScenario(
  "session export picks a session on a terminal",
  { config: quiet, turns: [say("Alpha reply."), say("Beta reply."), say("Gamma reply.")] },
  async (ctx) => {
    const ids = [
      sessionOf((await ctx.cli(["run", "--title", "Alpha session", "--format", "json", "Alpha"])).stdout),
      sessionOf((await ctx.cli(["run", "--title", "Beta session", "--format", "json", "Beta"])).stdout),
      sessionOf((await ctx.cli(["run", "--title", "Gamma session", "--format", "json", "Gamma"])).stdout),
    ]
    tails(ctx, ids)
    // The latest session comes first; the arrow keys move, Enter exports.
    await ctx.terminal(["session", "export"], [
      { waitFor: "Select session to export", type: "\x1b[B" },
      { waitFor: "● Beta session", type: "\x1b[B" },
      { waitFor: "● Alpha session", type: "\x1b[A" },
      { waitFor: "● Beta session", type: "\r" },
    ])
    // Typing filters by title, hint or ID.
    await ctx.terminal(["session", "export", "--sanitize"], [
      { waitFor: "Select session to export", type: "alp" },
      { waitFor: "(1 match)", type: "\r" },
    ])
    // A search without matches, then backspace and the arrows.
    await ctx.terminal(["session", "export"], [
      { waitFor: "Select session to export", type: "zzz" },
      { waitFor: "No matches found", type: "\x7f\x7f\x7f" },
      { waitFor: "Search: _", type: "\x1b[B" },
      { waitFor: "● Beta session", type: "\r" },
    ])
  },
)

cliScenario(
  "session export picker cancelled",
  { config: quiet, turns: [say("Reply.")] },
  async (ctx) => {
    const id = sessionOf((await ctx.cli(["run", "--title", "Only session", "--format", "json", "Hello"])).stdout)
    tails(ctx, [id])
    await ctx.terminal(["session", "export"], [
      { waitFor: "Select session to export", type: "On" },
      { waitFor: "Search: On█", type: "\x1b" },
    ])
    await ctx.terminal(["session", "export"], [{ waitFor: "Select session to export", type: "\x03" }])
  },
)

cliScenario("session export picker with no sessions", {}, async (ctx) => {
  await ctx.terminal(["session", "export"], [])
})

/** Rewrites each `toLocaleString()` of the session's update time, in each zone, to a placeholder naming the zone. */
function localTimes(ctx: CliCtx, updated: number, zones: readonly string[]) {
  const shown = zones.map((zone) => [new Date(updated).toLocaleString("en-US", { timeZone: zone }), zone] as const)
  ctx.scrub((text) => shown.reduce((out, [raw, zone]) => out.split(raw).join(`<time in ${zone}>`), text))
}

cliScenario(
  "session times in the local time zone",
  { env: { TZ: "Asia/Kolkata" }, config: quiet, turns: [say("Zoned reply.")] },
  async (ctx) => {
    const id = sessionOf((await ctx.cli(["run", "--title", "Zoned session", "--format", "json", "Hello"])).stdout)
    const listed = await ctx.cli(["session", "list", "--format", "json"])
    const updated = JSON.parse(listed.stdout)[0].updated as number
    localTimes(ctx, updated, ["Asia/Kolkata", "America/New_York", "Australia/Lord_Howe", "UTC"])
    tails(ctx, [id])
    await ctx.cli(["session", "list"])
    await ctx.cli(["session", "list"], { env: { TZ: "America/New_York" } })
    await ctx.cli(["session", "list"], { env: { TZ: "Australia/Lord_Howe" } })
    await ctx.cli(["session", "list"], { env: { TZ: "UTC" } })
    // The picker's hint shows the time in the local zone too (only while the prompt is active: the step waits for it).
    const hint = `(${new Date(updated).toLocaleString("en-US", { timeZone: "America/New_York" })} - ${id.slice(-8)})`
    await ctx.terminal(["session", "export"], [{ waitFor: hint, type: "\x03" }], { env: { TZ: "America/New_York" } })
  },
)
