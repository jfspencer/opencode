// `opencode upgrade`: the installation method, the update service (opencode.ai/update/api, answered by the fake
// network), and the package manager or installer it runs. Package managers are stubs on the scenario's PATH that
// log how they were called; nothing is installed. An executable installed by none of the known methods (the
// reference run from source, a Rust build) has no method: it needs `--method`. The curl installer's method is
// detected when `~/.opencode/bin/opencode` (in the sandbox's HOME) is the running executable.
import { chmodSync, mkdirSync, realpathSync, symlinkSync, writeFileSync } from "node:fs"
import path from "node:path"
import { cliScenario, type CliCtx } from "../lib/cli"
import type { Responder } from "../lib/openrouter"
import { implementation, runsTs } from "../lib/server"

/** A PATH of `<sandbox>/<dir>` (stub programs, each a shell script) before the inherited PATH, or alone. */
function stubs(ctx: CliCtx, dir: string, programs: Record<string, string>, inherit = true) {
  const bin = path.join(ctx.sandbox, dir)
  mkdirSync(bin, { recursive: true })
  for (const [name, script] of Object.entries(programs)) {
    writeFileSync(path.join(bin, name), script)
    chmodSync(path.join(bin, name), 0o755)
  }
  const inherited = process.env.PATH ?? ""
  ctx.scrub((text) => (inherited ? text.split(inherited).join("<PATH>") : text))
  return inherit ? `${bin}:${inherited}` : bin
}

/** A program that appends how it was called to `<sandbox>/calls.log`, then exits with `code`. */
const logging = (name: string, code = 0) => `#!/bin/sh\necho "${name} $*" >> "$(dirname "$0")/../calls.log"\nexit ${code}\n`
const managers = Object.fromEntries(["npm", "pnpm", "bun", "yarn", "vp", "brew"].map((name) => [name, logging(name)]))

// Temporary directories are named at random (`update-XXXXXX`).
const temporary = (ctx: CliCtx) => ctx.scrub((text) => text.replace(/update-[A-Za-z0-9]{6}/g, "update-<random>"))

/** The update service's answer for any channel, artifact and distribution. */
const release =
  (body: unknown, status = 200): Responder =>
  (request) =>
    request.url.startsWith("https://opencode.ai/update/api/") ? { status, body } : undefined
const latest = release({ version: "1.4.0", metadata: { package: "opencode-ai" } })

/** The executable under test: the reference runs inside Bun; a build is its own file. */
function executable() {
  const impl = implementation()
  return runsTs() || impl.kind !== "process" ? process.execPath : impl.command[0]
}

/** Makes `~/.opencode/bin/opencode` (the curl installer's binary) the running executable; recorded as `<executable>`. */
function curlInstall(ctx: CliCtx) {
  const target = executable()
  const bin = path.join(ctx.sandbox, "home", ".opencode", "bin")
  mkdirSync(bin, { recursive: true })
  symlinkSync(target, path.join(bin, "opencode"))
  const names = [...new Set([realpathSync(target), target])].sort((a, b) => b.length - a.length)
  ctx.scrub((text) => names.reduce((out, name) => out.split(name).join("<executable>"), text))
}

cliScenario("upgrade without a detectable installation method", {}, async (ctx) => {
  await ctx.cli(["upgrade"])
  await ctx.cli(["update"])
  await ctx.cli(["upgrade", "1.4.0"])
})

cliScenario("upgrade with each package manager", { responders: [latest] }, async (ctx) => {
  temporary(ctx)
  const env = { PATH: stubs(ctx, "bin", managers) }
  await ctx.cli(["upgrade", "--method", "npm"], { env })
  await ctx.cli(["upgrade", "v1.5.0-beta.1", "--method", "pnpm"], { env })
  await ctx.cli(["upgrade", "1.4.0", "-m", "bun"], { env })
  await ctx.cli(["update", "1.4.0", "--method", "yarn"], { env })
  await ctx.cli(["upgrade", "1.4.0", "--method", "vp"], { env })
  await ctx.cli(["upgrade", "--method", "brew"], { env })
  ctx.file("calls.log")
})

cliScenario(
  "upgrade skipped when the version is already installed",
  { responders: [release({ version: "conformance", metadata: { package: "opencode-ai" } })] },
  async (ctx) => {
    const env = { PATH: stubs(ctx, "bin", managers) }
    await ctx.cli(["upgrade", "conformance", "--method", "npm"], { env })
    await ctx.cli(["upgrade", " vconformance ", "--method", "npm"], { env })
    await ctx.cli(["upgrade", "--method", "bun"], { env })
    ctx.file("calls.log")
  },
)

cliScenario("upgrade with an invalid version", { responders: [latest] }, async (ctx) => {
  const env = { PATH: stubs(ctx, "bin", managers) }
  await ctx.cli(["upgrade", "not-a-version", "--method", "npm"], { env })
  await ctx.cli(["upgrade", "1.04.0", "--method", "npm"], { env })
  await ctx.cli(["upgrade", "1.4.0-beta.01", "--method", "npm"], { env })
  await ctx.cli(["upgrade", "99999999999999999.0.0", "--method", "npm"], { env })
  ctx.file("calls.log")
})

cliScenario("upgrade when the update service fails", { responders: [release({ error: "unavailable" }, 503)] }, async (ctx) => {
  const env = { PATH: stubs(ctx, "bin", managers) }
  await ctx.cli(["upgrade", "--method", "npm"], { env })
  // A target needs the service too: it names the package.
  await ctx.cli(["upgrade", "1.4.0", "--method", "npm"], { env })
})

cliScenario(
  "upgrade when the update service answers without a release",
  { responders: [release({ version: "1.4.0" })] },
  async (ctx) => {
    await ctx.cli(["upgrade", "--method", "npm"], { env: { PATH: stubs(ctx, "bin", managers) } })
  },
)

cliScenario("upgrade when the update service is unreachable", { responders: [() => { throw new Error("offline") }] }, async (ctx) => {
  await ctx.cli(["upgrade", "--method", "yarn"], { env: { PATH: stubs(ctx, "bin", managers) } })
})

cliScenario("upgrade when the package manager fails", { responders: [latest] }, async (ctx) => {
  // Long output keeps its last 12 lines, without terminal colors; no output says so.
  const noisy = [
    "#!/bin/sh",
    ...Array.from({ length: 14 }, (_, index) => `echo "npm \\033[31mERR!\\033[0m line ${index + 1} of the failure with enough words to wrap the detail" >&2`),
    "exit 1",
  ].join("\n")
  await ctx.cli(["upgrade", "--method", "npm"], { env: { PATH: stubs(ctx, "noisy", { npm: noisy }) } })
  await ctx.cli(["upgrade", "--method", "npm"], { env: { PATH: stubs(ctx, "quiet", { npm: "#!/bin/sh\nexit 3\n" }) } })
  await ctx.cli(["upgrade", "--method", "pnpm"], { env: { PATH: stubs(ctx, "stdout", { pnpm: "#!/bin/sh\necho 'ERR_PNPM_NO_GLOBAL_BIN_DIR Unable to find the global bin directory'\nexit 1\n" }) } })
  // The package manager is not installed.
  await ctx.cli(["upgrade", "--method", "npm"], { env: { PATH: stubs(ctx, "empty", {}, false) } })
})

cliScenario("upgrade through the curl installer", { responders: [latest] }, async (ctx) => {
  temporary(ctx)
  curlInstall(ctx)
  // The installer is whatever curl downloads: here a script that logs its arguments.
  const curl = [
    "#!/bin/sh",
    'echo "curl $*" >> "$(dirname "$0")/../calls.log"',
    'while [ "$1" != "-o" ]; do shift; done',
    `printf '#!/bin/sh\\necho "installer $*" >> "%s"\\n' "$(dirname "$0")/../calls.log" > "$2"`,
  ].join("\n")
  const env = { PATH: stubs(ctx, "bin", { curl }) }
  await ctx.cli(["upgrade"], { env })
  await ctx.cli(["upgrade", "v1.3.2"], { env })
  ctx.file("calls.log")
  // The download fails, then the installer does.
  await ctx.cli(["upgrade"], { env: { PATH: stubs(ctx, "offline", { curl: "#!/bin/sh\necho 'curl: (6) Could not resolve host: opencode.ai' >&2\nexit 6\n" }) } })
  const failing = '#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nprintf \'#!/bin/sh\\necho "installer: unsupported platform" >&2\\nexit 2\\n\' > "$2"\n'
  await ctx.cli(["upgrade", "1.4.0"], { env: { PATH: stubs(ctx, "failing", { curl: failing }) } })
})

cliScenario("upgrade through WinGet", {}, async (ctx) => {
  const winget = [
    "#!/bin/sh",
    'echo "winget $*" >> "$(dirname "$0")/../calls.log"',
    'if [ "$1" = "search" ]; then',
    "  echo 'Name     Id               Version Source'",
    "  echo '---------------------------------------'",
    "  echo 'OpenCode Anomaly.OpenCode 1.6.1   winget'",
    "fi",
  ].join("\n")
  await ctx.cli(["upgrade", "--method", "winget"], { env: { PATH: stubs(ctx, "bin", { winget }) } })
  await ctx.cli(["upgrade", "--method", "winget"], {
    env: { PATH: stubs(ctx, "nomatch", { winget: "#!/bin/sh\necho 'No package found matching input criteria.'\nexit 1\n" }) },
  })
  ctx.file("calls.log")
})
