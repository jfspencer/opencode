// `opencode uninstall`: what it lists, its confirmation, stopping the sandbox's background service and removing the
// sandbox's cache (HOME and the XDG directories are the scenario's: nothing outside the sandbox is touched). An
// executable installed by none of the known methods (the reference run from source, a Rust build) has no package
// to remove; the curl installer's method is detected when `~/.opencode/bin/opencode` is the running executable.
// Every command creates the cache's bin directory again, so the cache is checked right after each uninstall.
import { existsSync, mkdirSync, realpathSync, symlinkSync } from "node:fs"
import path from "node:path"
import { cliScenario, type CliCtx } from "../lib/cli"
import { implementation, runsTs } from "../lib/server"

/** Whether the sandbox's global cache directory exists. */
const cache = (ctx: CliCtx) => ctx.note("cache exists", existsSync(path.join(ctx.sandbox, "cache", "opencode")))

/**
 * Uninstall reuses one spinner for each step, and a spinner clears the last frame it drew when it stops, even one
 * drawn in an earlier step. Whether a step drew a frame depends on how long it took (a frame every 80 ms; lib/cli.ts
 * drops a frame with its clearing), so a clearing left without its frame is timing: dropped too.
 */
const spinnerTiming = (ctx: CliCtx) => ctx.scrub((text) => text.replaceAll("\x1b[999D\x1b[J", ""))

/** Makes `~/.opencode/bin/opencode` (the curl installer's binary) the running executable; recorded as `<executable>`. */
function curlInstall(ctx: CliCtx) {
  const impl = implementation()
  const target = runsTs() || impl.kind !== "process" ? process.execPath : impl.command[0]
  const bin = path.join(ctx.sandbox, "home", ".opencode", "bin")
  mkdirSync(bin, { recursive: true })
  symlinkSync(target, path.join(bin, "opencode"))
  const names = [...new Set([realpathSync(target), target])].sort((a, b) => b.length - a.length)
  ctx.scrub((text) => names.reduce((out, name) => out.split(name).join("<executable>"), text))
}

cliScenario("uninstall dry run lists what would be removed", {}, async (ctx) => {
  spinnerTiming(ctx)
  await ctx.cli(["uninstall", "--dry-run"])
  await ctx.cli(["service", "start"])
  await ctx.cli(["uninstall", "--dry-run"])
  cache(ctx)
  await ctx.cli(["service", "status"])
})

cliScenario("uninstall needs a terminal or --force", {}, async (ctx) => {
  await ctx.cli(["uninstall"])
  cache(ctx)
})

cliScenario("uninstall with --force stops the service and removes the cache", {}, async (ctx) => {
  spinnerTiming(ctx)
  await ctx.cli(["service", "start"])
  await ctx.cli(["uninstall", "--force"])
  cache(ctx)
  ctx.file("state/opencode/service-conformance.json")
  await ctx.cli(["service", "status"])
  // Nothing left to stop.
  await ctx.cli(["uninstall", "-f"])
  cache(ctx)
})

cliScenario("uninstall confirmed on a terminal", {}, async (ctx) => {
  await ctx.cli(["service", "start"])
  // The default answer is No; Enter declines, Ctrl+C cancels.
  await ctx.terminal(["uninstall"], [{ waitFor: "Are you sure you want to uninstall?", type: "\r" }])
  await ctx.terminal(["uninstall"], [{ waitFor: "Are you sure you want to uninstall?", type: "\x03" }])
  cache(ctx)
  await ctx.cli(["service", "status"])
  await ctx.terminal(["uninstall"], [
    { waitFor: "Are you sure you want to uninstall?", type: "\x1b[D" },
    { waitFor: "● Yes", type: "\r" },
  ])
  cache(ctx)
  await ctx.cli(["service", "status"])
})

cliScenario("uninstall answered with y on a terminal", {}, async (ctx) => {
  await ctx.terminal(["uninstall"], [{ waitFor: "Are you sure you want to uninstall?", type: "y" }])
  cache(ctx)
})

cliScenario("uninstall of a curl install", {}, async (ctx) => {
  spinnerTiming(ctx)
  curlInstall(ctx)
  await ctx.cli(["service", "start"])
  await ctx.cli(["uninstall", "--dry-run"])
  await ctx.cli(["uninstall", "--force"])
  cache(ctx)
  await ctx.cli(["service", "status"])
})
