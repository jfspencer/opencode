// The TS side of the config cases (cli.json decoding, Config.resolve and its keybind bindings, jsonc-parser's parse
// and modify, the CLI config service's get/update on real files), in the shape examples/tui-driver.rs prints.
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs"
import path from "node:path"

const reference = process.env.OPENCODE_TUI_REFERENCE!
const cli = path.join(reference, "packages/cli/src")
const work = process.env.OPENCODE_TUI_WORK!
const load = (specifier: string, from: string) => import(Bun.resolveSync(specifier, from))
const { Schema, Effect } = await load("effect", cli)
const { NodeFileSystem } = await load("@effect/platform-node", cli)
const jsonc = await load("jsonc-parser", cli)
const { Global } = await load("@opencode/util/global", cli)
const { Config: CliConfig } = await import(path.join(cli, "config/index.ts"))
const tuiConfig = await import(path.join(reference, "packages/tui/src/config/index.tsx"))
const { TuiKeybind } = await import(path.join(reference, "packages/tui/src/config/keybind.ts"))
const Config = tuiConfig.Config ?? tuiConfig

const decode = Schema.decodeUnknownOption(CliConfig.Info)

export type Op = { op: "set"; path: string[]; value: unknown } | { op: "delete"; path: string[] } | { op: "spread"; path: string[]; key: string; value: unknown }

export type Case =
  | { kind: "decode"; input: unknown }
  | { kind: "resolve"; input: unknown; unknown: string[]; terminalSuspend: boolean; herdr: boolean }
  | { kind: "parse"; text: string }
  | { kind: "modify"; text: string; path: (string | number)[]; value?: unknown; remove: boolean }
  | { kind: "service"; file: string | null; content: string | null; ops: Op[] | null }
  | { kind: "definitions" }

const guard = (fn: () => unknown) => {
  try {
    return { ok: fn() }
  } catch (error: any) {
    return { error: error instanceof Error ? error.message : String(error) }
  }
}

function resolvedJson(resolved: any) {
  return { ...resolved, keybinds: resolved.keybinds.bindings }
}

function apply(draft: any, ops: Op[]) {
  for (const op of ops) {
    let parent = draft
    for (const key of op.path.slice(0, -1)) {
      if (parent[key] === null || typeof parent[key] !== "object" || Array.isArray(parent[key])) parent[key] = {}
      parent = parent[key]
    }
    const last = op.path[op.path.length - 1]!
    if (op.op === "set") parent[last] = op.value
    else if (op.op === "delete") delete parent[last]
    // draft.x = { ...draft.x, key: value }, as the TUI's updates do.
    else parent[last] = { ...parent[last], [op.key]: op.value }
  }
}

let serviceRuns = 0
async function service(c: Extract<Case, { kind: "service" }>) {
  const directory = mkdtempSync(path.join(work, "ts-"))
  try {
    if (c.file !== null) writeFileSync(path.join(directory, "cli.json"), c.file)
    if (c.content === null) delete process.env.OPENCODE_CLI_CONFIG_CONTENT
    else process.env.OPENCODE_CLI_CONFIG_CONTENT = c.content
    serviceRuns++
    // A defect (jsonc-parser throwing on a document whose root is not an object) rejects with the error itself.
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const config = yield* CliConfig.Service
        if (!c.ops) return { ok: yield* config.get() }
        return yield* config.update((draft: any) => apply(draft, c.ops!)).pipe(
          Effect.map((value: unknown) => ({ ok: value })),
          Effect.catch((error: any) => Effect.succeed({ error: error.message })),
        )
      }).pipe(
        Effect.provide(CliConfig.layer),
        Effect.provide(Global.layerWith({ config: directory, state: directory })),
        Effect.provide(NodeFileSystem.layer),
      ),
    ).catch((error: any) => ({ error: error instanceof Error ? error.message : String(error) }))
    const file = path.join(directory, "cli.json")
    return { ...result, text: existsSync(file) ? readFileSync(file, "utf8") : null }
  } finally {
    delete process.env.OPENCODE_CLI_CONFIG_CONTENT
    rmSync(directory, { recursive: true, force: true })
  }
}

export async function runConfig(c: Case): Promise<unknown> {
  switch (c.kind) {
    case "decode": {
      const out = decode(c.input)
      return out._tag === "Some" ? JSON.stringify(out.value) : null
    }
    case "resolve":
      return guard(() => {
        const decoded = decode(c.input)
        const info: any = decoded._tag === "Some" ? structuredClone(decoded.value) : {}
        if (c.unknown.length) info.keybinds = { ...info.keybinds, ...Object.fromEntries(c.unknown.map((k) => [k, "x"])) }
        const resolved = Config.resolve(info, { terminalSuspend: c.terminalSuspend, environment: c.herdr ? { HERDR_ENV: "1" } : {} })
        const keybinds = resolved.keybinds
        const sample = ["app.exit", "session.list", "prompt.paste", "nope"]
        return {
          resolved: resolvedJson(resolved),
          get: sample.map((name) => keybinds.get(name)),
          has: sample.map((name) => keybinds.has(name)),
          gather: keybinds.gather("g", ["prompt.paste", "app.exit", "nope", "session.new"]),
          again: keybinds.gather("g", ["session.list"]),
          pick: keybinds.pick("g", ["session.new", "app.exit"]),
          omit: keybinds.omit("g", ["app.exit"]),
          omitMany: keybinds.omit("g", ["app.exit", "session.new"]),
          missing: keybinds.pick("h", ["app.exit"]),
        }
      })
    case "parse": {
      const errors: unknown[] = []
      const value = jsonc.parse(c.text, errors, { allowTrailingComma: true })
      return errors.length ? { errors: true } : { value: JSON.stringify(value) ?? null }
    }
    case "modify":
      return guard(() =>
        jsonc.applyEdits(
          c.text,
          jsonc.modify(c.text, c.path, c.remove ? undefined : c.value, { formattingOptions: { tabSize: 2, insertSpaces: true } }),
        ),
      )
    case "service":
      return service(c)
    case "definitions":
      return Object.entries(TuiKeybind.Definitions).map(([name, item]: [string, any]) => [name, item.default, item.description])
  }
}

// --- case generation --------------------------------------------------------------------------------------------
export function configCases(count: number, random: () => number): Case[] {
  const pick = <T,>(items: readonly T[]): T => items[Math.floor(random() * items.length)]!
  const int = (n: number) => Math.floor(random() * n)
  const names = Object.keys(TuiKeybind.Definitions)
  const bindingValues = [
    false, true, "none", "ctrl+q", "ctrl+c,<leader>q", "", [], ["a", "b"], ["a", { name: "b", ctrl: true }, { key: "c", desc: "d" }],
    { name: "q", ctrl: true, extra: 1 }, { key: "q", event: "press", zz: 1 }, { key: { name: "q", foo: 1 }, name: 5 },
    { key: 5 }, { key: "x", desc: "custom" }, { key: "x", preventDefault: false, fallthrough: true }, 7, null, { event: "press" },
  ]
  const leaves: Record<string, unknown[]> = {
    $schema: ["https://opencode.ai/v2/cli.json", 1],
    "theme.name": ["opencode", "dracula", "", 3], "theme.mode": ["system", "dark", "light", "auto"],
    "leader.timeout": [1, 1000, 2000.5, 0, -1, 1e300, "1000"], "scroll.speed": [0.001, 0.0009, 3, "3"], "scroll.acceleration": [true, 1],
    "attention.notifications": [true, false, "yes"], "attention.sound": [true], "attention.volume": [0, 0.4, 1, 1.5, -0.1],
    "attention.sound_pack": ["opencode.default", 2], "attention.sounds": [{ error: "x.wav", default: "y.wav" }, { nope: "z" }, { done: 5 }],
    "diffs.source": ["working", "branch", "committed", "turn", "other"], "diffs.wrap": ["word", "none", "char"], "diffs.view": ["auto", "split", "unified", "x"],
    "diffs.tree": [true, "no"], "terminal.title": [false, true], "terminal.copy": ["manual", "select", "auto"],
    "prompt.editor": [true], "prompt.paste": ["compact", "full", "x"], "prompt.image_preview": [false],
    "session.sidebar": ["auto", "hide", "show"], "session.tps": [false, true], "session.new_location": ["launch", "inherit", "x"],
    "session.permissions": ["prompt", "autoaccept"], "session.thinking": ["show", "hide"], "session.verbosity": ["low", "high", "max"],
    "tabs.mode": ["auto", "on", "off", "x"], "tabs.enabled": [true, false], "tabs.scope": ["global", "cwd"], "tabs.layout": ["horizontal", "vertical"],
    "tabs.indicators": ["status", "numbers"], "mini.work_spinner": ["seed", "nope"], "mini.replay_limit": [3, 0], "debug.turn_tokens": [true, "verbose", "x"],
    "debug.devtools": [true], experimental: [{ "new-designs": true }, { a: true, "10": false }, { a: 1 }], animations: [false, true, 0],
    highlights: [false], mouse: [false, true, "x"], "cursor.style": ["block", "underline", "line", "default", "bar"], "cursor.blinking": [false],
    plugins: [["a", { package: "b", options: { x: 1 } }], [{ package: "c", extra: 1 }], [{ options: {} }], "a"],
    theme: [[], null, "x"], tabs: [null], nope: [1],
  }
  const randomInfo = () => {
    const info: any = {}
    for (let n = int(6); n > 0; n--) {
      const key = pick(Object.keys(leaves))
      const value = pick(leaves[key]!)
      const parts = key.split(".")
      let target = info
      for (const part of parts.slice(0, -1)) {
        if (target[part] === null || typeof target[part] !== "object" || Array.isArray(target[part])) target[part] = {}
        target = target[part]
      }
      target[parts[parts.length - 1]!] = structuredClone(value)
    }
    if (random() < 0.4) {
      info.keybinds = {}
      for (let n = 1 + int(4); n > 0; n--) info.keybinds[random() < 0.95 ? pick(names) : "nope"] = structuredClone(pick(bindingValues))
    }
    return info
  }
  // A JSONC text of a value, in one of several layouts, with comments and trailing commas.
  const layout = (value: unknown) => {
    const style = int(5)
    if (style === 0) return JSON.stringify(value)
    if (style === 1) return JSON.stringify(value, null, 2)
    if (style === 2) return JSON.stringify(value, null, "\t")
    if (style === 3) return "// cli config\n" + JSON.stringify(value, null, 4).replace(/\n}$/, ",\n}") + "\n"
    return "{\n  /* block */ " + JSON.stringify(value, null, 2).slice(1).replace(/\n/g, "\r\n")
  }
  const out: Case[] = [{ kind: "definitions" }]
  // Hand-picked documents for jsonc-parser: comments, CRLF, tabs, nesting, first/last/only properties.
  const texts = [
    '{\n  "a": 1, // c\n  "b": 2\n}',
    '{\n  "a": 1 /* x */,\n  /* y */ "b": { "c": [1, 2,], },\n}\n',
    '{"theme": {"mode": "dark"}}',
    '{\r\n  "theme": {\r\n    "mode": "dark"\r\n  }\r\n}\r\n',
    '{\n\t"theme": {\n\t\t"mode": "dark"\n\t}\n}',
    '{ "only": true }',
    '{\n  "$schema": "https://opencode.ai/v2/cli.json"\n}\n',
    '  {\n    "nested": { "deep": { "x": 1 } }\n  }',
    '{}',
    '{\n}',
    '// top\n{\n  "a": [1, {"b": 2}]\n}',
    '{"a":1,"b":2,"c":3}',
  ]
  const paths: (string | number)[][] = [["a"], ["b"], ["c"], ["theme", "mode"], ["theme", "name"], ["only"], ["nested", "deep", "x"], ["nested", "deep", "y"], ["a", 1, "b"], ["a", 0], ["a", 5], ["a", -1], ["x", "y", "z"], []]
  for (const text of texts)
    for (const path of paths)
      for (const remove of [false, true]) out.push({ kind: "modify", text, path, value: { v: [1, "two"] }, remove })
  const broken = ['"abc', '"a\\u12x"', '"\\q"', '"a\tb"', '01', '-', '1e', '1.', '.5', '[1,]', '[,1]', '{,}', '{"a":}', '{"a" 1}', '{"a":1 "b":2}', '/* open', '// only', 'nul', 'true false', '{"__proto__": {"a": 1}, "b": 2}', '{"a": 1, "a": 2}', '[1e400, -0, 0.1]']
  for (const text of broken) out.push({ kind: "parse", text })
  for (let i = 0; i < count; i++) {
    const roll = random()
    if (roll < 0.25) out.push({ kind: "decode", input: randomInfo() })
    else if (roll < 0.45) {
      const unknown = random() < 0.2 ? (random() < 0.5 ? ["nope"] : ["a.b", "c"]) : []
      out.push({ kind: "resolve", input: randomInfo(), unknown, terminalSuspend: random() < 0.8, herdr: random() < 0.2 })
    } else if (roll < 0.6) {
      let text = layout(randomInfo())
      if (random() < 0.3) {
        const at = int(text.length + 1)
        text = text.slice(0, at) + pick(["", ",", "}", "/*", "//x\n", "\"", "-", "1.", "tru", "\\u12", " ", "\n"]) + text.slice(at)
      }
      out.push({ kind: "parse", text })
    } else if (roll < 0.8) {
      const text = random() < 0.1 ? pick(["", " ", "[]", "[1, 2]", "1", "{}", "{ }", "{\n}\n"]) : layout(randomInfo())
      const path = Array.from({ length: 1 + int(3) }, () => (random() < 0.08 ? int(3) : pick(["theme", "mode", "name", "keybinds", "app.exit", "tabs", "layout", "mouse", "x"])))
      const value = pick([true, "dark", 2, null, { a: 1, b: [1, 2] }, [], {}, "x\"y"])
      out.push({ kind: "modify", text, path, value, remove: random() < 0.25 })
    } else {
      const file = random() < 0.2 ? null : random() < 0.1 ? pick(["", "{", "[]", "{\"mouse\": 1}"]) : layout(randomInfo())
      const content = random() < 0.15 ? JSON.stringify(randomInfo()) : null
      const ops: Op[] | null =
        random() < 0.2
          ? null
          : Array.from({ length: 1 + int(3) }, () => {
              const r = random()
              if (r < 0.5)
                return pick<Op>([
                  { op: "spread", path: ["theme"], key: "mode", value: pick(["dark", "light", "system"]) },
                  { op: "spread", path: ["theme"], key: "name", value: pick(["opencode", "dracula"]) },
                  { op: "spread", path: ["session"], key: "sidebar", value: pick(["auto", "hide"]) },
                  { op: "spread", path: ["tabs"], key: "layout", value: "vertical" },
                  { op: "spread", path: ["prompt"], key: "paste", value: pick(["full", "compact"]) },
                  { op: "spread", path: ["diffs"], key: "wrap", value: "none" },
                ])
              if (r < 0.8) return { op: "set", path: [pick(["animations", "mouse", "highlights"])], value: pick([true, false, "bad"]) } as Op
              if (r < 0.9) return { op: "set", path: ["experimental", pick(["new-designs", "client-mcp"])], value: true } as Op
              return { op: "delete", path: [pick(["mouse", "theme", "keybinds", "animations"])] } as Op
            })
      out.push({ kind: "service", file, content, ops })
    }
  }
  return out
}

export const serviceCount = () => serviceRuns
