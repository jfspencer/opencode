// Preload for upstream's own tests: records every input they hand to the shell analysis modules, so the
// differential corpus covers exactly what upstream tests. Used by corpus.ts (`bun test --preload`); writes JSON
// lines to $SHELL_RECORD_FILE. The wrapped modules behave exactly as the originals.
import { mock } from "bun:test"
import { appendFileSync } from "node:fs"
import path from "node:path"

const core = path.resolve(process.env.SHELL_RECORD_CORE ?? "")
const file = process.env.SHELL_RECORD_FILE ?? ""
const seen = new Set<string>()
// Written at once: bun test exits without running process exit handlers.
const record = (value: unknown) => {
  const line = JSON.stringify(value)
  if (seen.has(line)) return
  seen.add(line)
  appendFileSync(file, line + "\n")
}

// mock.module updates a module already loaded in place, so the originals are captured first.
async function wrap(relative: string, wrapped: (original: Record<string, any>) => Record<string, any>, self: string) {
  const file = path.join(core, relative)
  const original = { ...(await import(file)) }
  const replacement = { ...original, ...wrapped(original) }
  replacement[self] = replacement
  mock.module(file, () => replacement)
}

await wrap(
  "src/shell/scan.ts",
  (original) => ({
    scan: (input: string, dialect?: string) => {
      if (typeof input === "string") record({ fn: "scan", input, dialect: dialect ?? "posix" })
      return original.scan(input, dialect)
    },
    scanPowerShell: (input: string) => {
      if (typeof input === "string") record({ fn: "scanPowerShell", input })
      return original.scanPowerShell(input)
    },
  }),
  "ShellScan",
)

await wrap(
  "src/util/wildcard.ts",
  (original) => ({
    match: (input: string, pattern: string) => {
      if (typeof input === "string" && typeof pattern === "string") record({ fn: "match", input, pattern })
      return original.match(input, pattern)
    },
    escape: (text: string) => {
      if (typeof text === "string") record({ fn: "escape", text })
      return original.escape(text)
    },
  }),
  "Wildcard",
)

await wrap(
  "src/shell/parse.ts",
  (original) => ({
    scan: (command: string, shell: string, cwd: string) => {
      if (typeof command === "string" && typeof shell === "string" && typeof cwd === "string")
        record({ fn: "parse", command, shell, cwd })
      return original.scan(command, shell, cwd)
    },
  }),
  "ShellParse",
)
