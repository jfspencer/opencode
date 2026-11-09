// The differential check of opencode-shell: every case runs through the TS reference modules and through the Rust
// crate (the shell-harness example), and the two JSON answers must be byte-identical.
//
// Corpus: (1) every input upstream's own tests hand to the shell modules (recorded by running them with
// record.ts), (2) every shell command the conformance goldens record and every string literal of the shell and
// permission scenarios, (3) generated inputs (generate.ts).
//
// usage: bun harness/driver.ts [--cases N] [--seed S] [--bin PATH] [--no-upstream] [--only FN] [--show N]
//   --cases N      generated cases per family (default 6000; the total corpus is about 13 N + the recorded ones)
//   --seed S       generator seed (default 1)
//   --bin PATH     the harness binary (default: build it with cargo, release)
//   --no-upstream  skip recording upstream's tests
//   --only FN      run only cases of one function (scan, parse, match, ...)
//   --show N       print the first N differences (default 10); all go to target/shell-harness/differences.jsonl
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs"
import os from "node:os"
import path from "node:path"
import {
  astralWildcardPair,
  directoryCommand,
  escapeTargeted,
  labelTargeted,
  large,
  memberTargeted,
  nested,
  oddTargeted,
  pathText,
  pick,
  powerShellCommandLine,
  random,
  shellCommand,
  sinkTargeted,
  spelledTargeted,
  wildcardPair,
  zshTargeted,
  type Random,
} from "./generate"
import { captureCases } from "./capture"

const args = process.argv.slice(2)
const option = (name: string) => {
  const index = args.indexOf(name)
  return index >= 0 ? args[index + 1] : undefined
}
const casesPerFamily = Number(option("--cases") ?? 6000)
const seed = Number(option("--seed") ?? 1)
const only = option("--only")
const show = Number(option("--show") ?? 10)

const crate = path.resolve(import.meta.dir, "..")
const port = path.resolve(crate, "../../..")
const workspace = path.join(port, "rust")
const reference = process.env.REFERENCE ?? path.join(port, ".reference")
const core = path.join(reference, "packages/core")
const out = path.join(workspace, "target/shell-harness")
mkdirSync(out, { recursive: true })

const load = (relative: string) => import(path.join(core, relative))
const { ShellScan } = await load("src/shell/scan.ts")
const { ShellParse } = await load("src/shell/parse.ts")
const { ShellSelect } = await load("src/shell/select.ts")
const { ShellResult } = await load("src/shell/result.ts")
const { Wildcard } = await load("src/util/wildcard.ts")
const { Permission } = await load("src/permission.ts")
const { FileAccess } = await load("src/file-access.ts")
const { Cause, Effect, Exit } = await import(Bun.resolveSync("effect", core))

type Case = { fn: string; [key: string]: unknown }
const cases: Case[] = []
const seen = new Set<string>()
const add = (value: Case) => {
  if (only && value.fn !== only) return
  const key = JSON.stringify(value)
  if (seen.has(key)) return
  seen.add(key)
  cases.push(value)
}

const DIALECTS = ["bash", "zsh", "posix"] as const
const SHELLS = ["/bin/bash", "/usr/bin/zsh", "/bin/sh", "/usr/bin/dash", "/usr/bin/pwsh", "powershell", "fish", "/opt/BASH"]
const CWDS = ["/workspace", "/home/user/project", "/"]

function addCommand(command: string, rand?: Random) {
  for (const dialect of DIALECTS) add({ fn: "scan", input: command, dialect })
  add({ fn: "scanPowerShell", input: command })
  const shells = rand ? [pick(rand, SHELLS)] : ["/bin/bash", "/bin/zsh", "/bin/sh", "/usr/bin/pwsh"]
  for (const shell of shells) add({ fn: "parse", command, shell, cwd: rand ? pick(rand, CWDS) : "/workspace" })
}

// (1) Upstream's tests, recorded.
function recordUpstream() {
  const files = [
    ...readdirSync(path.join(core, "test/shell-scan")).map((name) => `test/shell-scan/${name}`),
    ...readdirSync(path.join(core, "test"))
      .filter((name) => /^(shell|permission|tool-shell|session-shell)/.test(name) && name.endsWith(".test.ts"))
      .map((name) => `test/${name}`),
  ]
  const file = path.join(out, "upstream.jsonl")
  rmSync(file, { force: true })
  const started = performance.now()
  const run = Bun.spawnSync(["bun", "test", "--preload", path.join(import.meta.dir, "record.ts"), ...files], {
    cwd: core,
    env: { ...process.env, SHELL_RECORD_CORE: core, SHELL_RECORD_FILE: file },
    stdout: "pipe",
    stderr: "pipe",
    timeout: 600_000,
  })
  const summary = run.stderr.toString().match(/(\d+) pass[\s\S]*?(\d+) fail/)
  console.log(`upstream tests: ${files.length} files, ${summary ? `${summary[1]} pass, ${summary[2]} fail` : "no summary"} (${((performance.now() - started) / 1000).toFixed(1)}s)`)
  if (!existsSync(file)) throw new Error("upstream tests recorded nothing")
  const lines = readFileSync(file, "utf8").split("\n").filter(Boolean)
  for (const line of lines) {
    const record = JSON.parse(line)
    if (record.fn === "scan" || record.fn === "scanPowerShell" || record.fn === "parse") {
      addCommand(record.input ?? record.command)
      if (record.fn === "parse") add(record)
    } else add(record)
  }
  return lines.length
}

// The shell tool's own texts (its description, background and timeout results) come from functions the TS module
// does not export, so the goldens recorded from TS are their expected answers.
const goldenAnswers = new Map<string, string>()
const BACKGROUND_RE = /^Command moved to the background \(shell ID: ([^)]+)\)\.\nOutput is streaming to: ([^\n]+)(?:\n([\s\S]*))?$/
const TIMEOUT_RE = /^([\s\S]*)\n\nCommand exceeded timeout of (\d+) ms\. Retry with a larger timeout if the command is expected to take longer\.$/
function goldenText(text: string) {
  const golden = (value: Case, answer: string) => {
    if (only && value.fn !== only) return
    const key = JSON.stringify(value)
    if (goldenAnswers.has(key)) return
    goldenAnswers.set(key, JSON.stringify(answer))
    cases.push(value)
  }
  const description = /^Execute a shell command and return its output\.(?: Commands run on (\S+) using (\S+)\.)?/.exec(text)
  if (description && text.endsWith("you will be notified when they complete."))
    golden({ fn: "description", ...(description[2] ? { shell: description[2] } : {}), platform: "linux" }, text)
  const background = BACKGROUND_RE.exec(text)
  if (background) {
    golden({ fn: "background", shellID: background[1], file: background[2] }, text.split("\n").slice(0, 2).join("\n"))
    if (background[3] !== undefined) golden({ fn: "backgroundInstruction" }, background[3])
  }
  const timeout = TIMEOUT_RE.exec(text)
  if (timeout) golden({ fn: "timeoutOutput", output: timeout[1], timeout: Number(timeout[2]) }, text)
}

// (2) Every command a conformance golden records.
function scenarioCommands() {
  const goldens = path.join(port, "conformance/goldens")
  const commands = new Set<string>()
  const visit = (value: unknown) => {
    if (typeof value === "string") goldenText(value)
    if (typeof value === "string") {
      if (value.startsWith("{") && value.includes('"command"')) {
        try {
          visit(JSON.parse(value))
        } catch {}
      }
      return
    }
    if (Array.isArray(value)) return value.forEach(visit)
    if (value && typeof value === "object") {
      const record = value as Record<string, unknown>
      if (typeof record.command === "string") commands.add(record.command)
      Object.values(record).forEach(visit)
    }
  }
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const file = path.join(dir, name)
      if (statSync(file).isDirectory()) walk(file)
      else if (name.endsWith(".json")) visit(JSON.parse(readFileSync(file, "utf8")))
    }
  }
  walk(goldens)
  // The scenario sources too: shell-classify joins its lines into scripts, so each literal line is a case of its own.
  const ts = require(Bun.resolveSync("typescript", reference))
  for (const name of ["shell-scan", "shell-classify", "shell-tool", "permissions"]) {
    const file = path.join(port, `conformance/scenarios/${name}.test.ts`)
    const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true)
    const literal = (node: any): void => {
      if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) commands.add(node.text)
      ts.forEachChild(node, literal)
    }
    literal(source)
  }
  for (const command of commands) addCommand(command)
  return commands.size
}

// (3) Generated.
function generated() {
  const rand = random(seed)
  for (let i = 0; i < casesPerFamily * 3; i++) addCommand(shellCommand(rand), rand)
  for (let i = 0; i < casesPerFamily * 2; i++) {
    const command = powerShellCommandLine(rand)
    add({ fn: "scanPowerShell", input: command })
    add({ fn: "parse", command, shell: pick(rand, ["/usr/bin/pwsh", "pwsh", "C:/x/powershell"]), cwd: pick(rand, CWDS) })
  }
  for (let i = 0; i < casesPerFamily / 4; i++) {
    addCommand(nested(rand, false), rand)
    add({ fn: "scanPowerShell", input: nested(rand, true) })
    for (const command of [zshTargeted(rand), escapeTargeted(rand), sinkTargeted(rand), oddTargeted(rand)])
      addCommand(command, rand)
    for (const command of [labelTargeted(rand), memberTargeted(rand)]) {
      add({ fn: "scanPowerShell", input: command })
      add({ fn: "parse", command, shell: "/usr/bin/pwsh", cwd: "/workspace" })
    }
  }
  for (let i = 0; i < Math.min(60, casesPerFamily / 50); i++) {
    const text = large(rand)
    add({ fn: "scan", input: text, dialect: pick(rand, DIALECTS) })
    add({ fn: "scanPowerShell", input: text })
  }
  for (let i = 0; i < casesPerFamily; i++) {
    const powershell = rand() < 0.4
    add({
      fn: "parse",
      command: directoryCommand(rand, powershell),
      shell: powershell ? "/usr/bin/pwsh" : pick(rand, ["/bin/bash", "/bin/zsh", "/bin/sh"]),
      cwd: pick(rand, CWDS),
    })
  }
  for (let i = 0; i < casesPerFamily; i++) {
    const [input, pattern] = wildcardPair(rand)
    add({ fn: "match", input, pattern })
    add({ fn: "escape", text: input })
  }
  for (let i = 0; i < casesPerFamily / 4; i++) {
    const op = pick(rand, ["join", "normalize", "dirname", "basename", "resolve"])
    const count = op === "join" || op === "resolve" ? 1 + Math.floor(rand() * 3) : 1
    // resolve starts from an absolute directory, as the TS callers do, so process.cwd() is never read.
    const base = op === "resolve" ? [pick(rand, CWDS)] : []
    add({ fn: "path", op, args: [...base, ...Array.from({ length: count }, () => pathText(rand))] })
    add({ fn: "resolvePath", directory: pick(rand, CWDS), input: pick(rand, ["~", "~/", "~/a", "a", "../b", "/x/../y", pathText(rand)]), home: pick(rand, ["/home/u", "/home/u/", "/"]) })
    add({ fn: "select", file: pick(rand, [...SHELLS, "cmd", "C:/Windows/cmd.exe", "/bin/NU", "pwsh.exe", "", "/", "Pwsh", "/usr/bin/ksh"]), command: "echo hi" })
  }
  const effects = ["allow", "deny", "ask"]
  const patterns = ["*", "shell", "sh*", "?*", "read", "git *", "git status *", "*.env", "rm -rf *", "", "ls", "ls *", "[*]"]
  for (let i = 0; i < casesPerFamily / 2; i++) {
    const rules = Array.from({ length: Math.floor(rand() * 5) }, () => ({ action: pick(rand, patterns), resource: pick(rand, patterns), effect: pick(rand, effects) }))
    add({ fn: "evaluate", action: pick(rand, ["shell", "read", "edit", "*"]), resource: pick(rand, ["git status", "ls", "ls -la", "rm -rf /", ".env", "a.env", "", "*"]), rules })
  }
  const statuses = ["running", "exited", "timeout", "killed"]
  const maybe = <T>(value: T) => (rand() < 0.5 ? value : undefined)
  const compact = (value: Record<string, unknown>) => Object.fromEntries(Object.entries(value).filter(([, v]) => v !== undefined))
  for (let i = 0; i < casesPerFamily / 4; i++) {
    const info = compact({ id: "sh_1", command: "echo hi", status: pick(rand, statuses), exit: maybe(pick(rand, [0, 1, 2, 127, -1])), signal: maybe(pick(rand, ["SIGTERM", "SIGKILL"])) })
    const capture = maybe({ output: pick(rand, ["", "hi", "(no output)", "a\nb"]), truncated: rand() < 0.5 })
    add(compact({ fn: "output", info, capture }))
    add(compact({ fn: "userNotification", info, capture }))
    add(compact({ fn: "notice", exit: maybe(pick(rand, [0, 1, 3])), signal: maybe("SIGINT"), timeout: maybe(pick(rand, [true, false])) }))
    const output = maybe(compact({ output: "x", truncated: rand() < 0.5, exit: maybe(1), signal: maybe("SIGTERM"), timeout: maybe(pick(rand, [true, false])) }))
    add(compact({ fn: "notification", shellID: "sh_2", jobID: maybe("job_1"), command: pick(rand, ["ls", 'echo "x"']), state: pick(rand, ["completed", "cancelled", "error"]), text: pick(rand, ["", "out", "a\n\nb"]), output }))
  }
  for (let i = 0; i < casesPerFamily / 4; i++) {
    const bytes = Array.from({ length: Math.floor(rand() * 12) }, () => pick(rand, [0x41, 0x0a, 0x80, 0xbf, 0xc2, 0xc0, 0xe0, 0xe2, 0x82, 0xac, 0xed, 0xa0, 0xf0, 0x9f, 0x98, 0x80, 0xf4, 0x90, 0xff, Math.floor(rand() * 256)]))
    add({ fn: "utf8", bytes })
  }
  // Families added after the others, so the corpus before them stays as earlier mutation sweeps checked it.
  for (let i = 0; i < casesPerFamily / 2; i++) {
    const command = spelledTargeted(rand)
    addCommand(command, rand)
    add({ fn: "parse", command, shell: "/bin/bash", cwd: pick(rand, CWDS) })
    const [input, pattern] = astralWildcardPair(rand)
    add({ fn: "match", input, pattern })
  }
}

const started = performance.now()
const recorded = args.includes("--no-upstream") ? 0 : recordUpstream()
const fromScenarios = scenarioCommands()
const beforeGenerated = cases.length
generated()
// Shell.result's capture has no pure TS function: the TS Shell service answers these (capture.ts).
const precomputed = new Map<Case, string>()
if (!only || only === "capture") {
  for (const item of await captureCases(core, out, random(seed + 1), Math.max(30, casesPerFamily / 20))) {
    cases.push(item.case as Case)
    precomputed.set(item.case as Case, item.answer)
  }
}
console.log(`corpus: ${recorded} recorded upstream inputs, ${fromScenarios} scenario commands (goldens and sources), ${cases.length - beforeGenerated} generated cases; ${cases.length} cases in all`)

// The TS answers, in the canonical form the Rust harness writes.
const COMMAND_KEYS = new Set(["resource", "words", "rawWords", "wordEnds", "statementHead", "declaration", "redirectWordCount", "redirectPrefixEnd"])
function canonicalScan(result: any) {
  if (result.kind === "opaque") return JSON.stringify({ kind: "opaque", reason: result.reason })
  return JSON.stringify({
    kind: "scanned",
    commands: result.commands.map((command: any) => {
      for (const key of Object.keys(command)) if (!COMMAND_KEYS.has(key)) throw new Error(`unexpected command key ${key}`)
      return {
        resource: command.resource,
        words: command.words,
        rawWords: command.rawWords,
        ...(command.wordEnds !== undefined ? { wordEnds: command.wordEnds } : {}),
        ...(command.statementHead ? { statementHead: true } : {}),
        ...(command.declaration ? { declaration: true } : {}),
        ...(command.redirectWordCount !== undefined ? { redirectWordCount: command.redirectWordCount } : {}),
        ...(command.redirectPrefixEnd !== undefined ? { redirectPrefixEnd: command.redirectPrefixEnd } : {}),
      }
    }),
  })
}

function expected(value: Case): string {
  const answer = precomputed.get(value) ?? goldenAnswers.get(JSON.stringify(value))
  if (answer !== undefined) return answer
  switch (value.fn) {
    case "scan":
      return canonicalScan(ShellScan.scan(value.input, value.dialect))
    case "scanPowerShell":
      return canonicalScan(ShellScan.scanPowerShell(value.input))
    case "parse": {
      const exit = Effect.runSyncExit(ShellParse.scan(value.command, value.shell, value.cwd))
      if (Exit.isSuccess(exit)) return JSON.stringify({ ok: exit.value })
      return JSON.stringify({ error: (Cause.squash(exit.cause) as Error).message })
    }
    case "match":
      return JSON.stringify(Wildcard.match(value.input, value.pattern))
    case "escape":
      return JSON.stringify(Wildcard.escape(value.text))
    case "path":
      return JSON.stringify((path as any)[value.op as string](...(value.args as string[])))
    case "resolvePath":
      return JSON.stringify(FileAccess.resolvePath(value.directory, value.input, value.home))
    case "select":
      return JSON.stringify({
        name: ShellSelect.name(value.file),
        ps: ShellSelect.ps(value.file),
        login: ShellSelect.login(value.file),
        args: ShellSelect.args(value.file, value.command),
      })
    case "evaluate": {
      const rule = Permission.evaluate(value.action, value.resource, value.rules)
      return JSON.stringify({ action: rule.action, resource: rule.resource, effect: rule.effect })
    }
    case "output":
      return JSON.stringify(ShellResult.output({ info: value.info, capture: value.capture }))
    case "notice":
      return JSON.stringify(ShellResult.notice(value as any) ?? null)
    case "notification":
      return JSON.stringify(ShellResult.notification(value as any))
    case "userNotification":
      return JSON.stringify(ShellResult.userNotification({ info: value.info, capture: value.capture }))
    case "utf8":
      return JSON.stringify(Buffer.from(value.bytes as number[]).toString("utf8"))
    default:
      throw new Error(`unknown function ${value.fn}`)
  }
}

const tsStarted = performance.now()
const answers = cases.map((value) => expected(value))
const tsSeconds = (performance.now() - tsStarted) / 1000

// How the TS answers spread over outcomes, so a corpus that only reaches one branch shows.
if (args.includes("--stats")) {
  const outcomes = new Map<string, number>()
  cases.forEach((value, index) => {
    const answer = answers[index]!
    const outcome =
      value.fn === "scan" || value.fn === "scanPowerShell"
        ? `${value.fn}${value.dialect ? `/${value.dialect}` : ""} ${JSON.parse(answer).reason ?? `scanned(${Math.min(JSON.parse(answer).commands.length, 3)}${JSON.parse(answer).commands.length >= 3 ? "+" : ""})`}`
        : value.fn === "parse"
          ? `parse ${JSON.parse(answer).error?.replace("Portable shell scanner cannot analyze command: ", "error ") ?? `ok${JSON.parse(answer).ok.directories.length ? " +directories" : ""}`}`
          : `${value.fn} ${answer.length > 12 ? "…" : answer}`
    outcomes.set(outcome, (outcomes.get(outcome) ?? 0) + 1)
  })
  for (const [outcome, count] of [...outcomes].sort()) console.log(`  ${outcome.padEnd(48)} ${count}`)
}

// The Rust answers.
let bin = option("--bin")
if (!bin) {
  const build = Bun.spawnSync(["cargo", "build", "--quiet", "--release", "-p", "opencode-shell", "--example", "shell-harness"], { cwd: workspace, stdout: "inherit", stderr: "inherit" })
  if (build.exitCode !== 0) process.exit(build.exitCode ?? 1)
  bin = path.join(workspace, "target/release/examples/shell-harness")
}
const user = (() => {
  try {
    const info = os.userInfo()
    return { username: info.username, homedir: info.homedir }
  } catch {
    return null
  }
})()
const input = path.join(out, "cases.jsonl")
writeFileSync(input, [JSON.stringify({ home: os.homedir(), user, env: process.env }), ...cases.map((value) => JSON.stringify(value))].join("\n") + "\n")
const rustStarted = performance.now()
const run = Bun.spawnSync([bin], { stdin: Bun.file(input), stdout: "pipe", stderr: "pipe" })
const rustSeconds = (performance.now() - rustStarted) / 1000
if (run.exitCode !== 0) {
  console.error(run.stderr.toString())
  process.exit(1)
}
const rust = run.stdout.toString().split("\n")

const differences: Array<{ case: Case; ts: string; rust: string }> = []
const counts = new Map<string, { total: number; different: number }>()
cases.forEach((value, index) => {
  const count = counts.get(value.fn) ?? { total: 0, different: 0 }
  count.total++
  if (rust[index] !== answers[index]) {
    count.different++
    differences.push({ case: value, ts: answers[index]!, rust: rust[index] ?? "<missing>" })
  }
  counts.set(value.fn, count)
})
writeFileSync(path.join(out, "differences.jsonl"), differences.map((d) => JSON.stringify(d)).join("\n") + (differences.length ? "\n" : ""))
for (const [fn, count] of [...counts].sort()) console.log(`  ${fn.padEnd(18)} ${String(count.total).padStart(7)} cases  ${count.different} different`)
for (const difference of differences.slice(0, show)) {
  console.log(`\n${JSON.stringify(difference.case)}\n  ts:   ${difference.ts}\n  rust: ${difference.rust}`)
}
console.log(`\n${cases.length} cases, ${differences.length} differences (TS ${tsSeconds.toFixed(1)}s, Rust ${rustSeconds.toFixed(1)}s, total ${((performance.now() - started) / 1000).toFixed(1)}s)`)
rmSync(input, { force: true })
process.exit(differences.length ? 1 : 0)
