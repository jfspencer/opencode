// Generates versions, ranges and packuments (random, mutated and hand-picked) and records what the reference's own
// node-semver (the copy arborist resolves), npm-pick-manifest and npm-package-arg make of them.
//
// usage: bun semvercases.ts <reference node_modules/.bun dir> <seed> <count>
const [bunDir, seedArg, countArg] = process.argv.slice(2)
const arborist = `${bunDir}/@npmcli+arborist@9.4.0+759ce506b1ed1a42/node_modules`
const semver = require(`${arborist}/semver`)
const pickManifest = require(`${arborist}/npm-pick-manifest`)
const npa = require(`${arborist}/npm-package-arg`)

let seed = Number(seedArg ?? 1)
const count = Number(countArg ?? 20000)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T>(items: readonly T[]): T => items[Math.floor(random() * items.length)]
const chance = (p: number) => random() < p
const int = (n: number) => Math.floor(random() * n)

const numbers = ["0", "1", "2", "3", "10", "01", "007", "9007199254740991", "9007199254740992", "99999999999999999999", "123456789012345678901234567890"]
const ids = ["alpha", "beta", "rc", "0", "1", "2", "10", "a1", "1a", "-", "x", "0abc", "00", "01", "pre-1", "99999999999999999999", "B", "Z9"]
const xs = ["x", "X", "*"]
const spaces = ["", " ", "  ", "\t", " ", "﻿", " ", "\u0085", "\n"]

const num = () => (chance(0.8) ? String(int(4)) : pick(numbers))
const pre = () => Array.from({ length: 1 + int(3) }, () => pick(ids)).join(".")
const version = () => {
  let out = `${num()}.${num()}.${num()}`
  if (chance(0.3)) out += (chance(0.9) ? "-" : "") + pre()
  if (chance(0.15)) out += "+" + pick(["build", "001", "a.b", "x-y.1"])
  if (chance(0.1)) out = pick(["v", "=", "v=", " ", "=v", "vv"]) + out
  if (chance(0.05)) out = pick(spaces) + out + pick(spaces)
  return out
}
const partial = () => {
  const parts = [chance(0.85) ? num() : pick(xs)]
  if (chance(0.7)) parts.push(chance(0.7) ? num() : pick(xs))
  if (parts.length === 2 && chance(0.6)) parts.push(chance(0.7) ? num() : pick(xs))
  let out = parts.join(".")
  if (parts.length === 3 && chance(0.2)) out += "-" + pre()
  if (chance(0.05)) out += "+b"
  if (chance(0.05)) out = "v" + out
  return out
}
const operators = ["", "=", "<", ">", "<=", ">=", "~", "~>", "^", "==", "!", "<>", "=>", "=<"]
const comparator = () => {
  const op = pick(operators)
  return op + (chance(0.15) ? pick(spaces) : "") + (chance(0.7) ? partial() : version())
}
const simple = () => {
  if (chance(0.12)) return `${partial()} - ${partial()}`
  if (chance(0.05)) return pick(["*", "x", "", "latest", ">=0.0.0", ">=0.0.0-0", "<0.0.0-0", "^", "~", ">", "<"])
  return Array.from({ length: 1 + int(3) }, comparator).join(pick([" ", "  ", " ", "\t"]))
}
const range = () => {
  let out = Array.from({ length: 1 + int(3) }, simple).join(pick(["||", " || ", "|| ", " ||"]))
  if (chance(0.05)) out = pick(spaces) + out + pick(spaces)
  return out
}
const mutate = (text: string) => {
  const chars = [..."0123456789.-+xX*^~<>=| vab\t"]
  let out = text
  for (let i = 0; i < 1 + int(3); i++) {
    const at = int(out.length + 1)
    const kind = random()
    if (kind < 0.4) out = out.slice(0, at) + pick(chars) + out.slice(at)
    else if (kind < 0.8) out = out.slice(0, at) + out.slice(at + 1)
    else out = out.slice(0, at) + out.slice(at, at + 3) + out.slice(at)
  }
  return out
}

const options = () =>
  pick([undefined, true, false, { loose: true }, { includePrerelease: true }, { loose: true, includePrerelease: true }, {}])

const attempt = (f: () => unknown) => {
  try {
    return { value: f() }
  } catch (error) {
    return { throws: true }
  }
}

const cases: unknown[] = []
const push = (c: Record<string, unknown>) => cases.push(c)

const handVersions = ["1.2.3", "v1.2.3", "=1.2.3", " 1.2.3 ", "1.2", "1", "1.2.3-alpha", "1.2.3alpha", "1.2.3-01", "01.02.03", "1.2.3+build", "1.2.3-beta.1+b", "x.y.z", "", " ", "1.2.3-", "1.2.3--", "9007199254740992.0.0", "1.2.3-9007199254740991", "1.2.3-0099999999999999999999"]
const handRanges = ["^1.2.3", "~1.2.3", "1.x", "1.2.x", ">=1.2.3 <2", "1.2.3 - 2.3.4", "1.2 - 2.3", "* || 1.2.3", "<0.0.0-0", ">=0.0.0", "^0.0.1", "^0.1.2", "~0.0.1", "^1.2.3-beta", "~1.2.3-beta.2", ">1", ">=1.x", "<=1.2", "<1.2.3-0", "1.2.3 || 2.x", "", "*", "x", "latest", "^ 1.2.3", "~ 1.2.3", "> 1.2.3", ">= 1.2.3 <= 2.0.0", "1.2.3+build", "^1.2.3+build", "~> 1.2", "=1.2.3", "v1.2.3", "1.2.3 - x", "x - 1.2.3", "^99999999999999999999.0.0", "~1.99999999999999999999", "1.2.3 1.2.4", ">=1.0.0-rc.1 <1.0.1"]

for (const v of handVersions) {
  for (const opts of [true, { includePrerelease: true }, undefined]) {
    push({ op: "valid", version: v, options: opts, result: attempt(() => semver.valid(v, opts)) })
    push({ op: "clean", version: v, options: opts, result: attempt(() => semver.clean(v, opts)) })
  }
  for (const r of handRanges)
    for (const opts of [true, { includePrerelease: true }, { loose: true, includePrerelease: true }, undefined])
      push({ op: "satisfies", version: v, range: r, options: opts, result: attempt(() => semver.satisfies(v, r, opts)) })
}
for (const r of handRanges)
  for (const opts of [true, { includePrerelease: true }, undefined])
    push({ op: "validRange", range: r, options: opts, result: attempt(() => semver.validRange(r, opts)) })

for (let i = 0; i < count; i++) {
  const opts = options()
  const kind = random()
  if (kind < 0.55) {
    const v = chance(0.1) ? mutate(version()) : version()
    const r = chance(0.15) ? mutate(range()) : range()
    push({ op: "satisfies", version: v, range: r, options: opts, result: attempt(() => semver.satisfies(v, r, opts)) })
  } else if (kind < 0.75) {
    const r = chance(0.2) ? mutate(range()) : range()
    push({ op: "validRange", range: r, options: opts, result: attempt(() => semver.validRange(r, opts)) })
  } else if (kind < 0.85) {
    const v = chance(0.2) ? mutate(version()) : version()
    push({ op: "valid", version: v, options: opts, result: attempt(() => semver.valid(v, opts)) })
    push({ op: "clean", version: v, options: opts, result: attempt(() => semver.clean(v, opts)) })
  } else if (kind < 0.92) {
    const a = version()
    const b = chance(0.3) ? a : version()
    push({ op: "compare", a, b, options: opts, result: attempt(() => semver.compare(a, b, opts)) })
  } else {
    const name = pick(["a", "@s/b", "c-d"])
    const spec = pick([range(), version(), partial(), "latest", "next", "beta", "*", "", "npm:x@1", "github:a/b", "a/b", "git+file:///r#main", "file:../x", "https://x.test/a.tgz", "not valid", "1.2.3 || foo"])
    push({ op: "npa", name, spec, result: attempt(() => {
      const r = npa.resolve(name, spec, "/w")
      // A hosted Git spec is only recognized (installing one needs the host), so only its type is compared.
      if (r.hosted) return { type: r.type, hosted: true }
      return { type: r.type, fetchSpec: r.fetchSpec ?? null, saveSpec: r.saveSpec ?? null }
    }) })
  }
}

// npm-pick-manifest: random packuments and wanted specs.
const tags = ["latest", "next", "beta", "old"]
const packument = () => {
  const versions: Record<string, unknown> = {}
  const time: Record<string, string> = {}
  const list = Array.from({ length: 1 + int(7) }, () => (chance(0.85) ? `${int(3)}.${int(3)}.${int(3)}${chance(0.2) ? "-" + pick(["beta", "rc.1", "0", "alpha.2"]) : ""}` : pick(["1.0", "v1.0.0", "not-a-version", "1.0.0-x+y"])))
  for (const v of list) {
    const manifest: Record<string, unknown> = { name: "pkg", version: v }
    if (chance(0.2)) manifest.deprecated = pick(["old", "", true])
    if (chance(0.2)) manifest.engines = pick([{ node: ">=30" }, { node: ">=8" }, { node: "^26.0.0" }, { npm: ">=99" }, { node: "" }, { node: 5 }, "node >= 4"])
    if (chance(0.3)) manifest.bin = pick(["bin.js", { pkg: "./cli.js", "../x": "a/../b", ":y": "\\w" }, ["a/b.js", "c"], { z: 1 }, ""])
    versions[v] = manifest
    if (chance(0.5)) time[v] = pick(["2020-01-01T00:00:00.000Z", "2024-06-01T00:00:00.000Z", "2026-01-01", "garbage"])
  }
  const distTags: Record<string, unknown> = {}
  for (const t of tags) if (chance(0.6)) distTags[t] = chance(0.9) ? pick(list) : pick(["9.9.9", 3])
  const out: Record<string, unknown> = { name: "pkg", "dist-tags": distTags, versions }
  if (chance(0.5)) out.time = time
  if (chance(0.1)) out.policyRestrictions = { versions: { [pick(list)]: versions[list[0]] }, message: "no" }
  return out
}
for (let i = 0; i < Math.ceil(count / 5); i++) {
  const p = packument()
  const wanted = pick(["", "*", "latest", "next", "beta", "^1.0.0", "~1.1.0", "1.x", ">=1.0.0-0", "1.0.0", "v1.1.1", "2", "<=2.2.2", "0.x || 2.x", "nope", ">3"])
  const before = chance(0.2) ? pick(["2021-01-01", "2025-01-01T00:00:00Z", "garbage"]) : undefined
  push({
    op: "pick",
    packument: p,
    wanted,
    before: before ?? null,
    result: attempt(() => pickManifest(structuredClone(p), wanted, before ? { before } : {})),
  })
}

process.stdout.write(JSON.stringify(cases))
