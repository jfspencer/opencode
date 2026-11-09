// A reload that cannot boot every location again (packages/server/src/handlers/location.ts): the failure is a 503
// ServiceUnavailableError whose message is Effect's `Cause.pretty` of the boot failure. That text is the error's
// `<name>: <message>` line followed by the JavaScript stack trace, whose frames are the host's (source paths,
// engine internals, and in-process the harness's own frames), so the reply is recorded with the message's first
// line only.
import { chmodSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { scenario, type Ctx } from "../lib/scenario"

// The locations that did boot announce their inventory in the background; when the reload fails, the project's
// location is asked for again and awaited, so the recording does not end before (or between) its announcements.
async function reload(ctx: Ctx) {
  const from = ctx.events.length
  const response = await ctx.handler(new Request("http://opencode.local/api/location/reload", { method: "POST" }))
  const body = await response.json().catch(() => null)
  const message = typeof body?.message === "string" ? body.message.split("\n")[0] : body?.message
  ctx.note("POST /api/location/reload", { status: response.status, body: body && { ...body, message } })
  if (response.status === 204) return
  await ctx.http("GET", "/api/location")
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === ctx.project, from)
}

async function load(ctx: Ctx, directory: string) {
  await ctx.http("GET", "/api/location", undefined, { "x-opencode-directory": encodeURIComponent(directory) })
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === directory)
}

scenario("reload after a location's directory went away", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const other = path.join(path.dirname(ctx.project), `gone-${path.basename(ctx.project)}`)
  mkdirSync(other, { recursive: true })
  await load(ctx, other)
  rmSync(other, { recursive: true, force: true })
  await reload(ctx)
})

scenario("reload after a location's directory became unreadable", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const other = path.join(path.dirname(ctx.project), `locked-${path.basename(ctx.project)}`)
  mkdirSync(other, { recursive: true })
  await load(ctx, other)
  chmodSync(other, 0o000)
  try {
    await reload(ctx)
  } finally {
    chmodSync(other, 0o755)
    rmSync(other, { recursive: true, force: true })
  }
})

// The same check guards every request: an unreadable directory is not a location.
scenario("a request for an unreadable directory", { files: { "a.txt": "a\n" } }, async (ctx) => {
  const other = path.join(path.dirname(ctx.project), `unreadable-${path.basename(ctx.project)}`)
  mkdirSync(other, { recursive: true })
  chmodSync(other, 0o000)
  try {
    await ctx.http("GET", "/api/location", undefined, { "x-opencode-directory": encodeURIComponent(other) })
  } finally {
    chmodSync(other, 0o755)
    rmSync(other, { recursive: true, force: true })
  }
})

scenario("reload with a broken project config", { files: { "a.txt": "a\n" }, server: { filewatcher: false } }, async (ctx) => {
  await load(ctx, ctx.project)
  writeFileSync(path.join(ctx.project, "opencode.json"), '{ "model": ')
  await reload(ctx)
  await ctx.http("GET", "/api/config")
})
