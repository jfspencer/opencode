// Mutation check of the harness itself: random operator mutants of the crate, each built and run against a quick
// corpus. A surviving mutant is a behavior the corpus does not reach (or an equivalent mutant); survivors are
// listed for review. Every file is restored after its mutant, but do not edit the crate while this runs.
//
// usage: bun harness/mutants.ts [count=40] [seed=1] [driver options, default: --no-upstream --cases 2500]
//   count "all" runs every site once, in order instead of at random.
//   MUTANTS_ONLY=src/parse.rs:130-160,src/wildcard.rs keeps only the sites in those files and line ranges (1-based,
//   inclusive), e.g. the lines a change touched.
import { readFileSync, writeFileSync } from "node:fs"
import path from "node:path"

const crate = path.resolve(import.meta.dir, "..")
const workspace = path.resolve(crate, "../..")
const files = ["src/scan/bash.rs", "src/scan/powershell.rs", "src/parse.rs", "src/word.rs", "src/wildcard.rs", "src/path.rs"]
const only = (process.env.MUTANTS_ONLY ?? "")
  .split(",")
  .filter(Boolean)
  .map((item) => {
    const [file, range] = item.split(":")
    const [from, to] = (range ?? "1-1000000").split("-").map(Number)
    return { file, from: from!, to: to ?? from! }
  })
const all = process.argv[2] === "all"
let state = Number(process.argv[3] ?? 1)
const driverArgs = process.argv.length > 4 ? process.argv.slice(4) : ["--no-upstream", "--cases", "2500"]
const rand = () => (state = (state * 1103515245 + 12345) % 2147483648) / 2147483648
const bin = path.join(workspace, "target/release/examples/shell-harness")

const swaps: Array<[RegExp, string]> = [
  [/ \+ 1\b/, " + 2"], [/ - 1\b/, " - 2"], [/ < /, " <= "], [/ <= /, " < "], [/ > /, " >= "], [/ >= /, " > "],
  [/ == /, " != "], [/ != /, " == "], [/ && /, " || "], [/ \|\| /, " && "], [/\btrue\b/, "false"], [/\bfalse\b/, "true"],
  [/\.is_some\(\)/, ".is_none()"], [/\.is_empty\(\)/, ".len() > 0"], [/\bbreak;/, "continue;"],
]
const sites: Array<{ file: string; line: number; swap: [RegExp, string] }> = []
for (const file of files) {
  readFileSync(path.join(crate, file), "utf8")
    .split("\n")
    .forEach((text, line) => {
      if (/^\s*(\/\/|#\[|use |pub use|mod )/.test(text)) return
      if (only.length && !only.some((item) => item.file === file && line + 1 >= item.from && line + 1 <= item.to)) return
      for (const swap of swaps) if (swap[0].test(text)) sites.push({ file, line, swap })
    })
}
const count = all ? sites.length : Number(process.argv[2] ?? 40)
console.log(`${sites.length} mutation sites, ${count} mutants`)
const survivors: string[] = []
let killed = 0
let unbuilt = 0
for (let i = 0; i < count; i++) {
  const site = all ? sites[i]! : sites[Math.floor(rand() * sites.length)]!
  const file = path.join(crate, site.file)
  const original = readFileSync(file, "utf8")
  const lines = original.split("\n")
  const before = lines[site.line]!
  lines[site.line] = before.replace(site.swap[0], site.swap[1])
  writeFileSync(file, lines.join("\n"))
  try {
    const build = Bun.spawnSync(["cargo", "build", "--quiet", "--release", "-p", "opencode-shell", "--example", "shell-harness"], { cwd: workspace, stderr: "pipe" })
    if (build.exitCode !== 0) {
      unbuilt++
      continue
    }
    const run = Bun.spawnSync(["bun", "harness/driver.ts", ...driverArgs, "--show", "0", "--bin", bin], { cwd: crate, stdout: "pipe", stderr: "pipe", timeout: 600_000 })
    const differences = Number(run.stdout.toString().match(/(\d+) differences/)?.[1] ?? -1)
    if (differences === 0) survivors.push(`${site.file}:${site.line + 1}: ${before.trim()}  =>  ${lines[site.line]!.trim()}`)
    else killed++
  } finally {
    writeFileSync(file, original)
  }
}
Bun.spawnSync(["cargo", "build", "--quiet", "--release", "-p", "opencode-shell", "--example", "shell-harness"], { cwd: workspace })
console.log(`killed ${killed}, survived ${survivors.length}, did not build ${unbuilt}`)
for (const survivor of survivors) console.log("  " + survivor)
