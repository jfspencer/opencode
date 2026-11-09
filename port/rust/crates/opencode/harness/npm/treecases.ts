// Whole installs, TS against Rust: each case is a generated registry (packuments and gzipped tarballs served on
// loopback, with or without a content-length) and an npm config, and the package `prettier` is installed from it by
// the reference's `Npm.which` and by the Rust port (the `install_driver` test), each into fresh caches. The two
// must answer the same bin, leave the same cache directory (paths, file contents, executable bits, links; the
// generation's timestamp and staging name masked) and send the same requests (as a multiset: arborist fetches
// concurrently).
//
// usage: bun treecases.ts --driver <rust test binary> --reference <port/.reference> --work <dir> [--seed S] [--count N] [--show N]
import fs from "node:fs"
import path from "node:path"
import { gzipSync } from "node:zlib"

const args = new Map<string, string>()
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ""), process.argv[i + 1])
const driver = args.get("driver")!
const reference = args.get("reference")!
const work = path.join(args.get("work")!, "tree")
let seed = Number(args.get("seed") ?? 1)
const count = Number(args.get("count") ?? 200)
const show = Number(args.get("show") ?? 5)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T>(items: readonly T[]): T => items[Math.floor(random() * items.length)]
const chance = (p: number) => random() < p
const int = (n: number) => Math.floor(random() * n)

type Files = Record<string, string | { mode: number; content: string }>
const tar = (files: Files) => {
  const blocks: Buffer[] = []
  for (const [name, entry] of Object.entries(files)) {
    const content = Buffer.from(typeof entry === "string" ? entry : entry.content)
    const mode = typeof entry === "string" ? 0o644 : entry.mode
    const header = Buffer.alloc(512)
    header.write(`package/${name}`, 0)
    header.write(`${mode.toString(8).padStart(7, "0")}\0`, 100)
    header.write("0000000\0", 108)
    header.write("0000000\0", 116)
    header.write(`${content.length.toString(8).padStart(11, "0")}\0`, 124)
    header.write("00000000000\0", 136)
    header.write("        ", 148)
    header.write("0", 156)
    header.write("ustar\0", 257)
    header.write("00", 263)
    let sum = 0
    for (const byte of header) sum += byte
    header.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148)
    blocks.push(header, content, Buffer.alloc((512 - (content.length % 512)) % 512))
  }
  blocks.push(Buffer.alloc(1024))
  return gzipSync(Buffer.concat(blocks))
}

const names = ["prettier", "a", "b", "c", "d", "@s/x", "@s/y"]
const versions = ["1.0.0", "1.1.0", "1.2.0", "2.0.0", "2.1.0-beta.1", "3.0.0"]
const ranges = ["^1.0.0", "~1.1.0", "*", "^2.0.0", ">=1.0.0 <2.0.0", "1.x", "latest", "2.1.0-beta.1", "^3", "1.0.0 || 2.0.0", "^1.1.0", ">=2"]

interface Case {
  registry: Record<string, { "dist-tags": Record<string, string>; versions: Record<string, Record<string, unknown>> }>
  npmrc: string
  chunked: boolean
}

const makeCase = (): Case => {
  const registry: Case["registry"] = {}
  const lists = new Map<string, string[]>()
  for (const name of names) {
    if (name !== "prettier" && chance(0.04)) continue
    const list = [...new Set(Array.from({ length: 1 + int(4) }, () => pick(versions)))]
    if (name === "prettier" && !list.includes("3.0.0")) list.push("3.0.0")
    lists.set(name, list)
  }
  // a range for a dependency on `dep`: mostly one its versions satisfy
  const rangeFor = (dep: string) => {
    const list = lists.get(dep)
    if (!list || chance(0.25)) return pick(ranges)
    const version = pick(list)
    const [major, minor] = version.split(".")
    return pick([version, `^${version}`, `~${version}`, `${major}.x`, `>=${major}.${minor}.0`, `${major}`, `<=${version}`])
  }
  for (const [name, list] of lists) {
    const entry: Case["registry"][string] = { "dist-tags": { latest: name === "prettier" ? "3.0.0" : pick(list) }, versions: {} }
    if (chance(0.2)) entry["dist-tags"].next = pick(list)
    for (const version of list) {
      const manifest: Record<string, unknown> = {}
      const files: Files = { "index.js": `module.exports = ${JSON.stringify(`${name}@${version}`)}\n` }
      const others = names.filter((n) => n !== name && n !== "prettier")
      const deps = (n: number) => {
        const out: Record<string, string> = {}
        for (let i = 0; i < n; i++) {
          const dep = pick(others)
          out[dep] = chance(0.04) ? pick(["npm:@s/x@^1", "not a range!", `http://127.0.0.1:PORT/tarballs/remote-${version}.tgz`]) : rangeFor(dep)
        }
        return out
      }
      const depCount = name === "prettier" ? 1 + int(3) : int(3)
      if (depCount) manifest.dependencies = deps(depCount)
      if (chance(0.25)) manifest.optionalDependencies = deps(1 + int(2))
      if (chance(0.2)) {
        manifest.peerDependencies = deps(1 + int(2))
        if (chance(0.4)) manifest.peerDependenciesMeta = { [Object.keys(manifest.peerDependencies as object)[0]]: { optional: true } }
      }
      if (chance(0.06)) manifest.os = pick([["linux"], ["!linux"], ["darwin"], "linux"])
      if (chance(0.05)) manifest.cpu = pick([["x64"], ["arm64"], ["!arm64"]])
      if (chance(0.08)) manifest.engines = pick([{ node: ">=30" }, { node: ">=8" }])
      if (chance(0.08)) manifest.deprecated = "old"
      if (chance(0.3)) manifest.license = pick(["MIT", { type: "ISC" }, "Apache-2.0"])
      if (chance(0.1)) manifest.funding = "https://example.test/fund"
      if (name === "prettier") {
        manifest.bin = { prettier: "bin.sh" }
        files["bin.sh"] = { mode: 0o755, content: "#!/bin/sh\necho prettier\n" }
      } else if (chance(0.3)) {
        const bin = pick(["cli.js", "./bin/x.js", "missing.js"])
        manifest.bin = chance(0.5) ? bin : { [`${name.replace("@s/", "")}-cli`]: bin }
        if (bin !== "missing.js") files[bin.replace("./", "")] = chance(0.3) ? "#!/usr/bin/env node\r\nrun()\r\n" : "#!/bin/sh\nexit 0\n"
      }
      if (chance(0.1)) files[".gitignore"] = "node_modules\n"
      if (chance(0.05)) files[".npmignore"] = "x\n"
      manifest.files = files
      entry.versions[version] = manifest
    }
    registry[name] = entry
  }
  const npmrc = pick([
    "",
    "",
    "",
    "legacy-peer-deps=true\n",
    "install-strategy=nested\n",
    "omit=optional\n",
    "os=darwin\n",
    "//127.0.0.1:PORT/:_authToken=secret-token\n",
    "@s:registry=http://127.0.0.1:PORT/scoped/\n//127.0.0.1:PORT/scoped/:_auth=dXNlcjpwYXNz\n",
    "prefer-dedupe=true\n",
    "global-style=true\n",
    "strict-peer-deps=true\n",
    "registry=http://127.0.0.1:PORT/mirror/\nreplace-registry-host=always\n",
  ])
  return { registry, npmrc, chunked: chance(0.5) }
}

const serve = (spec: Case) => {
  const tarballs = new Map<string, Buffer>()
  const requests: string[] = []
  let origin = ""
  const packument = (name: string) => {
    const entry = spec.registry[name]
    const out: Record<string, unknown> = {}
    for (const [version, manifest] of Object.entries(entry.versions)) {
      const { files, ...rest } = manifest as { files: Files }
      const body = tar({ "package.json": JSON.stringify({ name, version, ...rest }, null, 2), ...files })
      const tarball = `/${name}/-/${name.split("/").pop()}-${version}.tgz`
      tarballs.set(tarball, body)
      const integrity = `sha512-${new Bun.CryptoHasher("sha512").update(body).digest("base64")}`
      out[version] = JSON.parse(JSON.stringify({ name, version, ...rest, dist: { tarball: `${origin}${tarball}`, integrity } }).replaceAll("PORT", new URL(origin).port))
    }
    return { name, "dist-tags": entry["dist-tags"], versions: out }
  }
  for (const version of versions) tarballs.set(`/tarballs/remote-${version}.tgz`, tar({ "package.json": JSON.stringify({ name: "remote", version }) }))
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      const url = new URL(request.url)
      const pathname = decodeURIComponent(url.pathname).replace(/^\/(scoped|mirror)/, "")
      requests.push(JSON.stringify([request.method, url.pathname, request.headers.get("accept"), request.headers.get("authorization")]))
      const body = (data: Uint8Array | string) =>
        spec.chunked ? new Response(new Blob([data]).stream(), { headers: { "cache-control": "no-store" } }) : new Response(data, { headers: { "cache-control": "no-store" } })
      const tarball = tarballs.get(pathname)
      if (tarball) return body(tarball)
      const name = pathname.slice(1)
      if (!spec.registry[name]) return new Response("{}", { status: 404 })
      return body(JSON.stringify(packument(name)))
    },
  })
  origin = `http://127.0.0.1:${server.port}`
  for (const name of Object.keys(spec.registry)) packument(name)
  return { server, requests, port: server.port }
}

const walk = (root: string) => {
  const out: Record<string, string> = {}
  const visit = (dir: string, rel: string) => {
    for (const name of fs.readdirSync(dir).sort()) {
      const full = path.join(dir, name)
      const key = rel ? `${rel}/${name}` : name
      const stat = fs.lstatSync(full)
      if (stat.isSymbolicLink()) out[key] = `-> ${fs.readlinkSync(full)}`
      else if (stat.isDirectory()) {
        out[key] = "dir"
        visit(full, key)
      } else {
        const exec = stat.mode & 0o100 ? "x " : "  "
        out[key] = exec + fs.readFileSync(full, "utf8").replace(/\.staging-\d+-[0-9a-f-]{36}/g, "<staging>")
      }
    }
  }
  if (fs.existsSync(root)) visit(root, "")
  return out
}

const run = async (command: string[], dir: string, spec: Case, port: number) => {
  const env: Record<string, string> = {}
  for (const [key, value] of Object.entries(process.env)) if (value !== undefined && !/proxy|^npm_/i.test(key)) env[key] = value
  Object.assign(env, {
    HOME: path.join(dir, "home"),
    XDG_CACHE_HOME: path.join(dir, "cache"),
    XDG_STATE_HOME: path.join(dir, "state"),
    XDG_DATA_HOME: path.join(dir, "data"),
    XDG_CONFIG_HOME: path.join(dir, "config"),
    NPM_REFERENCE: reference,
    NPM_DRIVER_PKG: "prettier",
  })
  for (const key of ["home", "cache", "state", "data", "config"]) fs.mkdirSync(path.join(dir, key), { recursive: true })
  // Nothing may reach the real registry: a case without its own `registry=` gets the local one.
  const npmrc = `${/^registry=/m.test(spec.npmrc) ? "" : `registry=http://127.0.0.1:${port}/\n`}${spec.npmrc}`.replaceAll("PORT", String(port))
  fs.writeFileSync(path.join(dir, "home", ".npmrc"), npmrc)
  const child = Bun.spawn(command, { env, cwd: dir, stdout: "pipe", stderr: "pipe" })
  const stdout = await new Response(child.stdout).text()
  await child.exited
  const which = (stdout.match(/WHICH:(.*)/)?.[1] ?? "<none>").replace(dir, "<dir>").replace(/\/\d{13,}\//, "/<generation>/")
  const cache = path.join(dir, "cache", "opencode", "npm", "prettier@latest")
  const generations = fs.existsSync(cache) ? fs.readdirSync(cache).filter((n) => /^\d+$/.test(n)) : []
  return { which, generations: generations.length, files: generations.length ? walk(path.join(cache, generations[0])) : {} }
}

fs.rmSync(work, { recursive: true, force: true })
fs.mkdirSync(work, { recursive: true })
let differ = 0
let installed = 0
for (let i = 0; i < count; i++) {
  const spec = makeCase()
  const { server, requests, port } = serve(spec)
  const ts = await run(["bun", path.join(import.meta.dir, "which.ts"), "prettier"], path.join(work, `${i}-ts`), spec, port)
  const tsRequests = requests.splice(0).sort()
  const rust = await run([driver, "npm::tests::install_driver", "--ignored", "--exact", "--nocapture", "--test-threads=1"], path.join(work, `${i}-rust`), spec, port)
  const rustRequests = requests.splice(0).sort()
  server.stop(true)
  if (ts.generations) installed++
  const a = JSON.stringify({ ...ts, requests: tsRequests }, null, 1)
  const b = JSON.stringify({ ...rust, requests: rustRequests }, null, 1)
  if (a !== b) {
    differ++
    if (differ <= show) {
      fs.writeFileSync(path.join(work, `${i}.case.json`), JSON.stringify(spec, null, 1))
      fs.writeFileSync(path.join(work, `${i}.ts.json`), a)
      fs.writeFileSync(path.join(work, `${i}.rust.json`), b)
      console.log(`case ${i} differs: ${path.join(work, `${i}`)}.{case,ts,rust}.json`)
    }
  } else {
    fs.rmSync(path.join(work, `${i}-ts`), { recursive: true, force: true })
    fs.rmSync(path.join(work, `${i}-rust`), { recursive: true, force: true })
  }
}
console.log(`${differ} of ${count} installs differ (${installed} installed, the rest failed in TS)`)
process.exit(differ ? 1 : 0)
