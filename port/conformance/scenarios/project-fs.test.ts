// Location filesystem routes: reading files (confinement, ranges, conditional requests), listing directories,
// fuzzy finding (ignore rules), writing files, and watch/unwatch.
import { mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import path from "node:path"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

const at = (directory: string) => ({ "x-opencode-directory": encodeURIComponent(directory) })

const files = {
  "README.md": "# Demo\n\nA small project.\n",
  "src/math.ts": "export const add = (a: number, b: number) => a + b\n",
  "src/strings.ts": "export const greet = (name: string) => `hello ${name}`\n",
  "src/nested/deep/value.ts": "export const value = 42\n",
  "docs/guide.md": "Guide.\n",
  "docs/with space.md": "Spaced name.\n",
  "assets/pixel.png": "\x89PNG\r\n\x1a\nfake",
  "data/empty.txt": "",
  ".hidden/secret.txt": "hidden\n",
  ".env": "TOKEN=1\n",
  "dist/bundle.js": "ignored\n",
  "app.log": "ignored log\n",
  ".gitignore": "dist/\n*.log\n",
}

scenario("read files", { files }, async (ctx) => {
  await ctx.http("GET", "/api/fs/read/README.md")
  await ctx.http("GET", "/api/fs/read/src/nested/deep/value.ts")
  await ctx.http("GET", "/api/fs/read/docs/with%20space.md")
  await ctx.http("GET", "/api/fs/read/data/empty.txt")
  await ctx.http("GET", "/api/fs/read/.hidden/secret.txt")
  await ctx.http("GET", "/api/fs/read/dist/bundle.js")
})

scenario("read errors and confinement", { files }, async (ctx) => {
  mkdirSync(path.join(path.dirname(ctx.project), path.basename(ctx.project) + "-outside"), { recursive: true })
  writeFileSync(path.join(path.dirname(ctx.project), path.basename(ctx.project) + "-outside", "secret.txt"), "outside\n")
  symlinkSync(path.join(path.dirname(ctx.project), path.basename(ctx.project) + "-outside", "secret.txt"), path.join(ctx.project, "escape.txt"))
  symlinkSync(path.join(ctx.project, "README.md"), path.join(ctx.project, "inside-link.md"))
  await ctx.http("GET", "/api/fs/read/missing.txt")
  await ctx.http("GET", "/api/fs/read/README.md/x")
  await ctx.http("GET", "/api/fs/read/src")
  await ctx.http("GET", "/api/fs/read/src/../../outside.txt")
  await ctx.http("GET", "/api/fs/read/..%2F..%2Fetc%2Fhostname")
  await ctx.http("GET", "/api/fs/read/escape.txt")
  await ctx.http("GET", "/api/fs/read/inside-link.md")
  await ctx.http("GET", "/api/fs/read/bad%00name")
  await ctx.http("GET", "/api/fs/read/%E0%A4%A")
  await ctx.http("GET", `/api/fs/read/${encodeURIComponent(path.join(ctx.project, "README.md"))}`)
})

scenario("read with byte ranges", { files }, async (ctx) => {
  const range = (value: string, extra: Record<string, string> = {}) =>
    ctx.http("GET", "/api/fs/read/README.md", undefined, { range: value, ...extra })
  await range("bytes=0-5")
  await range("bytes=2-")
  await range("bytes=-6")
  await range("bytes=10-1000")
  await range("bytes=1000-")
  await range("bytes=5-2")
  await range("bytes=-0")
  await range("bytes=0-1,4-5")
  await range("bytes=-")
  await range("lines=1-2")
  await ctx.http("GET", "/api/fs/read/data/empty.txt", undefined, { range: "bytes=-1" })
  await range("bytes=0-3", { "if-range": '"not-the-etag"' })
  await range("bytes=0-3", { "if-range": "Thu, 01 Jan 2026 00:00:00 GMT" })
  await range("bytes=0-3", { "if-range": "Fri, 02 Jan 2026 00:00:00 GMT" })
})

scenario("conditional reads", { files }, async (ctx) => {
  const read = (headers: Record<string, string>) => ctx.http("GET", "/api/fs/read/README.md", undefined, headers)
  await read({ "if-none-match": "*" })
  await read({ "if-none-match": 'W/"0-0", "other"' })
  await read({ "if-modified-since": "Wed, 01 Jan 2031 00:00:00 GMT" })
  await read({ "if-modified-since": "Wed, 01 Jan 2020 00:00:00 GMT" })
  await read({ "if-modified-since": "not a date" })
})

scenario("list directories", { files: { ...files, "sub/inner/file.txt": "x\n" } }, async (ctx) => {
  symlinkSync(path.join(ctx.project, "README.md"), path.join(ctx.project, "link.md"))
  await ctx.http("GET", "/api/fs/list")
  await ctx.http("GET", "/api/fs/list?path=src")
  await ctx.http("GET", "/api/fs/list?path=src/nested/")
  await ctx.http("GET", `/api/fs/list?path=${encodeURIComponent(path.join(ctx.project, "docs"))}`)
  await ctx.http("GET", "/api/fs/list?path=missing")
  await ctx.http("GET", "/api/fs/list?path=README.md")
  await ctx.http("GET", "/api/fs/list?path=README.md/x")
  // From a nested location, a parent directory can be listed; entries stay relative to the location.
  await ctx.http("GET", "/api/fs/list?path=..", undefined, at(path.join(ctx.project, "sub")))
  await ctx.http("GET", "/api/fs/list?path=inner", undefined, at(path.join(ctx.project, "sub")))
  // Two locations booted here; wait for the nested one too so its boot events are complete.
  await ctx.until((event) => event?.type === "plugin.updated" && event?.location?.directory === path.join(ctx.project, "sub"))
})

scenario("find files and directories", { files, git: true }, async (ctx) => {
  await ctx.http("GET", "/api/fs/find?query=math")
  await ctx.http("GET", "/api/fs/find?query=src&type=directory")
  await ctx.http("GET", "/api/fs/find?query=ts&type=file")
  await ctx.http("GET", "/api/fs/find?query=ts&limit=2")
  await ctx.http("GET", "/api/fs/find?query=bundle")
  await ctx.http("GET", "/api/fs/find?query=secret")
  await ctx.http("GET", "/api/fs/find?query=zzzzqqq")
  await ctx.http("GET", "/api/fs/find?query=x&limit=0")
  await ctx.http("GET", "/api/fs/find?query=x&type=socket")
})

scenario("find in a directory without version control", { files }, async (ctx) => {
  await ctx.http("GET", "/api/fs/find?query=log")
  await ctx.http("GET", "/api/fs/find?query=deep")
})

scenario("find from the home directory", {}, async (ctx) => {
  const home = process.env.HOME!
  mkdirSync(path.join(home, "zqxj-home-probe"), { recursive: true })
  writeFileSync(path.join(home, "zqxj-home-probe", "zqxj-file.txt"), "x\n")
  try {
    // The query is unique to this scenario's files: HOME is shared by every scenario in the process.
    await ctx.http("GET", "/api/fs/find?query=zqxjhomeprobe", undefined, at(home))
    await ctx.http("GET", "/api/location", undefined, at(home))
  } finally {
    rmSync(path.join(home, "zqxj-home-probe"), { recursive: true, force: true })
  }
})

scenario("write files", { files }, async (ctx) => {
  await ctx.http("POST", "/api/experimental/fs/write?path=notes/new.txt", "plain text body", {
    "content-type": "application/octet-stream",
  })
  await ctx.http("POST", "/api/experimental/fs/write?path=README.md", "replaced", {
    "content-type": "application/octet-stream",
  })
  await ctx.http("POST", `/api/experimental/fs/write?path=${encodeURIComponent(path.join(ctx.project, "abs/written.txt"))}`, "absolute", {
    "content-type": "application/octet-stream",
  })
  await ctx.http("POST", "/api/experimental/fs/write?path=sub/../flattened.txt", "dots", {
    "content-type": "application/octet-stream",
  })
  await ctx.http("POST", "/api/experimental/fs/write", "no path", { "content-type": "application/octet-stream" })
  await ctx.http("GET", "/api/fs/read/notes/new.txt")
})

scenario("watch and unwatch files", { files }, async (ctx) => {
  await ctx.http("POST", "/api/experimental/fs/watch", { path: "README.md" })
  await ctx.http("POST", "/api/experimental/fs/watch", { path: "missing.txt" })
  await ctx.http("POST", "/api/experimental/fs/watch", { path: "README.md/x" })
  await ctx.http("POST", "/api/experimental/fs/watch", { path: "../outside.txt" })
  await ctx.http("DELETE", "/api/experimental/fs/watch", { path: "README.md" })
  await ctx.http("DELETE", "/api/experimental/fs/watch", { path: "never-watched.txt" })
})
