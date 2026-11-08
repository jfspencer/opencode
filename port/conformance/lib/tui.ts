// TUI driver. Runs the opencode TUI connected over HTTP to the scenario's server, exactly as in production, drives it
// with keystrokes, and records screen frames into the scenario's transcript. Server-side effects (provider requests,
// events, files) are recorded as usual.
//
// The TUI's terminal is always Ghostty's terminal core (lib/terminal.ts): the TUI's output is interpreted by it, its
// queries (device attributes, modes, kitty keyboard flags, color scheme) get its answers, keys reach the TUI as its
// key encoder writes them for the keyboard protocol the TUI turned on, and frames are read from its screen. Where
// the TUI runs follows CONFORMANCE_IMPL (lib/server.ts):
//   in-process   the TS reference's TUI (packages/tui `run`) in the test process, on an opentui test renderer
//                created with the TUI's own renderer options, drawing on demand like it, and attached to the
//                terminal (`bun test --coverage` measures what the scenarios reach). Run these from
//                port/conformance/tui: its bunfig.toml adds the SolidJS JSX transform.
//   a process    the implementation's CLI, `opencode --server <url>`, in a pseudo-terminal attached to the terminal.
//                "ts" runs the TS reference this way, which is what proves this path against the goldens.
//
// In-process the TUI gets what `opencode --server <url>` passes it (packages/cli/src/commands/handlers/default.ts);
// a process gets the same through its public interface:
//   app identity            the build's (version and channel "conformance", see lib/server.ts)
//   server                  --server <the recording proxy below> (it carries the server's credentials itself)
//   args.continue           --continue
//   args.sessionID          --session <id> (the CLI passes an existing session as sessionID, a missing one as
//   args.newSessionID         newSessionID: scenarios give each one only in the case the CLI produces)
//   args.prompt             --prompt <text>
//   args.auto               --auto
//   args.model/agent/fork   no CLI flag reaches them: scenarios that use them are in-process only
//   config (+ animations)   <XDG_CONFIG_HOME>/opencode/cli.json, written before start (later changes persist there)
//   global directories      HOME (the sandbox's) and XDG_* under <root>/tui/<project>
//   updater                 OPENCODE_DISABLE_AUTOUPDATE=1: no update check of its own (it is a network call); the
//                           update command is listed but not exercised
// and the fake network (HTTP(S)_PROXY, loopback direct) for anything it would send off the machine.
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
import { Writable } from "node:stream"
import { fakeNetwork } from "./network"
import { scenario, type Ctx, type ScenarioOptions } from "./scenario"
import { implementation, opencodeCommand, runsTs, status, tsEnv } from "./server"
import { PtyTerminal, VirtualTerminal, type Modifiers } from "./terminal"

export interface TuiOptions extends ScenarioOptions {
  readonly width?: number
  readonly height?: number
  /** TUI arguments (continue, sessionID, newSessionID, prompt, auto; in-process also model, agent, fork). */
  readonly args?: Record<string, unknown>
  /** TUI config (cli.json). Animations are always off: they are timing, not behavior. */
  readonly tui?: Record<string, unknown>
  /**
   * Runs against the server before the TUI starts (e.g. create sessions to open with --session/--continue).
   * Whatever it returns is merged into `args`.
   */
  readonly setup?: (ctx: Ctx) => Promise<Record<string, unknown> | void>
  /** Record which endpoints the TUI called (default). Off only where that set races in the TUI itself. */
  readonly requests?: boolean
}

export interface TuiCtx extends Ctx {
  type(text: string): Promise<void>
  /**
   * Press a key: a single character, an opentui KeyCodes name (RETURN, ARROW_UP, F8...), a key name (enter, tab,
   * backspace, delete, escape, space, up, down, left, right, home, end, pageup, pagedown, f1-f12) or the legacy
   * sequence of one of those keys.
   */
  press(key: string, modifiers?: Modifiers): Promise<void>
  enter(): Promise<void>
  escape(): Promise<void>
  tab(): Promise<void>
  arrow(direction: "up" | "down" | "left" | "right"): Promise<void>
  paste(text: string): Promise<void>
  /** Wait until the screen contains the text (or matches), then return the frame. */
  waitFor(match: string | RegExp, timeout?: number): Promise<string>
  /** Capture the screen once it has stopped changing and record it under a label. Throws once the TUI has exited. */
  frame(label: string): Promise<string>
  resize(width: number, height: number): Promise<void>
  click(x: number, y: number): Promise<void>
  scroll(x: number, y: number, direction: "up" | "down" | "left" | "right"): Promise<void>
}

/** A running TUI and its screen. */
interface Screen {
  /** The screen as text; throws once the TUI has exited. */
  capture(): string
  /** The hyperlinks on the screen (`text -> uri`). */
  links(): string[]
  /** Let the TUI render (in-process: render one frame; a process renders on its own). */
  render(): Promise<void>
  /** Wait (briefly) for a frame that passes the test. */
  match(test: (frame: string) => boolean): Promise<boolean>
  type(text: string): Promise<void>
  press(key: string, modifiers?: Modifiers): Promise<void>
  paste(text: string): Promise<void>
  click(x: number, y: number): Promise<void>
  scroll(x: number, y: number, direction: "up" | "down" | "left" | "right"): Promise<void>
  resize(width: number, height: number): Promise<void>
  close(): Promise<void>
}

export function tuiScenario(name: string, options: TuiOptions, body: (t: TuiCtx) => Promise<void>) {
  scenario(name, { timeout: 90_000, ...options }, async (ctx) => {
    const requests: string[] = []
    const server = Bun.serve({
      port: 0,
      // The TUI holds its event stream (and long requests) open for as long as the scenario runs.
      idleTimeout: 0,
      fetch: async (request) => {
        const url = new URL(request.url)
        requests.push(`${request.method} ${url.pathname}`)
        const response = await ctx.handler(request)
        // CONFORMANCE_TUI_LOG=1: every request the TUI makes, with its status, on stderr (debugging).
        if (process.env.CONFORMANCE_TUI_LOG)
          console.error(`tui ${request.method} ${url.pathname}${url.search} -> ${response.status}`)
        return response
      },
    })
    const args = { ...options.args, ...((options.setup ? await options.setup(ctx) : undefined) ?? {}) }
    const config: Record<string, unknown> = structuredClone({ ...options.tui, animations: false })
    const size = { width: options.width ?? 100, height: options.height ?? 30 }
    const screen =
      implementation().kind === "process"
        ? await terminalScreen(ctx, server.url.toString(), args, config, size)
        : await inProcessScreen(ctx, server.url.toString(), args, config, size)

    const stable = async () => {
      let last = ""
      let same = 0
      // Five identical captures (~75 ms): asynchronous rendering (markdown, a reloaded transcript) can pause longer
      // than a shorter window under load.
      for (let i = 0; i < 200 && same < 5; i++) {
        await screen.render()
        await new Promise((resolve) => setTimeout(resolve, 15))
        const frame = screen.capture()
        same = frame === last ? same + 1 : 0
        last = frame
      }
      return last
    }
    // After an input: the TUI reacts asynchronously (its renderer draws on demand, a process reads its terminal on
    // its own schedule), so first give it a moment to change the screen at all, then wait for the screen to settle.
    // An input that changes nothing costs the whole moment.
    const after = async (action: () => Promise<void>) => {
      const before = (() => {
        try {
          return screen.capture()
        } catch {
          return undefined
        }
      })()
      await action()
      for (let waited = 0; before !== undefined && waited < REACTION; waited += 10) {
        await new Promise((resolve) => setTimeout(resolve, 10))
        try {
          if (screen.capture() !== before) break
        } catch {
          break
        }
      }
      await stable()
    }
    const t: TuiCtx = {
      ...ctx,
      type: (text) => after(() => screen.type(text)),
      press: (key, modifiers) => after(() => screen.press(key, modifiers)),
      enter: () => after(() => screen.press("enter")),
      escape: () => after(() => screen.press("escape")),
      tab: () => after(() => screen.press("tab")),
      arrow: (direction) => after(() => screen.press(direction)),
      paste: (text) => after(() => screen.paste(text)),
      async waitFor(match, timeout = 20_000) {
        const test = (frame: string) => (typeof match === "string" ? frame.includes(match) : match.test(frame))
        // A single wait gives up after a fixed number of frames; retry until the timeout.
        const deadline = Date.now() + timeout
        while (true) {
          if (await screen.match(test)) return stable()
          if (Date.now() > deadline) throw new Error(`waitFor: screen never matched ${String(match)}`)
          await new Promise((resolve) => setTimeout(resolve, 25))
        }
      },
      async frame(label) {
        const frame = await stable()
        ctx.note(
          `frame:${label}`,
          frame.split("\n").map((line) => untimed(line).trimEnd()),
        )
        // Where the terminal shows a hyperlink only its text is on the screen: the targets are recorded beside it.
        const links = screen.links()
        if (links.length) ctx.note(`links:${label}`, links)
        return frame
      },
      resize: (width, height) => after(() => screen.resize(width, height)),
      click: (x, y) => after(() => screen.click(x, y)),
      scroll: (x, y, direction) => after(() => screen.scroll(x, y, direction)),
    }
    try {
      await stable()
      await body(t)
    } finally {
      await screen.close()
      await server.stop(true)
    }
    // Which endpoints the TUI writes to is part of its contract with the server; concurrent order is not. Reads
    // are not recorded: the client's data layer refetches on bursts of events and reconciles events that
    // overtake responses, so which reads it makes is timing (and the frames show what it loaded).
    if (options.requests !== false)
      ctx.note("tui-requests", [...new Set(requests.filter((request) => !request.startsWith("GET ")))].sort())
  })
}

async function inProcessScreen(
  ctx: Ctx,
  url: string,
  args: Record<string, unknown>,
  initial: Record<string, unknown>,
  size: { width: number; height: number },
): Promise<Screen> {
  const [{ createTestRenderer }, { Effect, FileSystem }, { Global }, { run }] = await Promise.all([
    import("@opentui/core/testing"),
    import("effect"),
    import("@opencode/util/global"),
    import("../ref/packages/tui/src/app"),
  ])
  // The renderer's terminal is Ghostty's, as for a process: its output goes in, the terminal's answers to its
  // queries come back as input (after the write, as through a pseudo-terminal).
  let deliver = (_: string) => {}
  const terminal = await VirtualTerminal.create(size.width, size.height, (response) =>
    setImmediate(() => deliver(response)),
  )
  const raw = process.env.CONFORMANCE_TUI_RAW
  const recorded: Uint8Array[] = []
  const sent: string[] = []
  const stdout = Object.assign(
    new Writable({
      write(chunk, _encoding, callback) {
        if (raw) recorded.push(Buffer.from(chunk))
        terminal.write(chunk)
        callback()
      },
    }),
    { columns: size.width, rows: size.height, isTTY: true },
  )
  // The renderer options the TUI creates its own renderer with (packages/tui/src/app.tsx; the TUI applies
  // useMouse itself), on the alternate screen like a real renderer, writing to the terminal. The console overlay
  // stays off: here it would capture the whole test process's console.
  const setup = await createTestRenderer({
    width: size.width,
    height: size.height,
    stdout: stdout as never,
    bufferedOutput: "stdout" as never,
    screenMode: "alternate-screen",
    externalOutputMode: "passthrough",
    targetFps: 60,
    gatherStats: false,
    exitOnCtrlC: false,
    useKittyKeyboard: {},
    autoFocus: false,
    openConsoleOnError: false,
    consoleOptions: { keyBindings: [{ name: "y", ctrl: true, action: "copy-selection" }] },
  })
  const input = (data: string) => {
    if (!data) return
    if (raw) sent.push(data)
    setup.renderer.stdin.emit("data", Buffer.from(data))
  }
  deliver = input
  // Rendering on demand, as the TUI's renderer does (it never starts a continuous render loop): a frame is
  // drawn when something asks for one. A test renderer's forced frames would lay out what production would not.
  await setup.renderer.setupTerminal()
  const state = path.join(process.env.CONFORMANCE_ROOT!, "tui", path.basename(ctx.project))
  for (const dir of ["home", "data", "cache", "config", "state", "tmp"])
    mkdirSync(path.join(state, dir), { recursive: true })
  let config = initial
  const ready = Promise.withResolvers<void>()
  const previous = process.cwd()
  // The TUI opens the project it is started in.
  process.chdir(ctx.project)
  const task = Effect.runPromise(
    run({
      app: { name: "cli", version: "conformance", channel: "conformance", highlights: [] },
      // As `opencode --server <url>` passes it: a remote server, an updater that never checks on its own
      // (OPENCODE_DISABLE_AUTOUPDATE=1 for processes) and whose manual check is not exercised.
      server: { endpoint: { url }, local: false },
      updater: {
        remote: true,
        subscribe: () => () => {},
        check: async () => ({ type: "unavailable", message: "update checks are not exercised" }),
        apply: async () => {
          throw new Error("updates are not exercised")
        },
      },
      config: {
        get: async () => config as never,
        update: async (mutate: (draft: never) => void) => {
          const next = structuredClone(config)
          mutate(next as never)
          config = next
          return next as never
        },
      },
      packages: { prepare: async () => ({ directory: "" }) },
      // No theme mode: the TUI asks the terminal, as it does when it creates its own renderer.
      terminalHandoff: async () => ({ renderer: setup.renderer, mode: null, complete: ready.resolve }),
      args: args as never,
      log: () => {},
    } as never).pipe(
      Effect.provide(
        Global.layerWith({
          home: process.env.HOME!,
          data: path.join(state, "data"),
          cache: path.join(state, "cache"),
          config: path.join(state, "config"),
          state: path.join(state, "state"),
          bin: path.join(state, "cache", "bin"),
          log: path.join(state, "data", "log"),
          repos: path.join(state, "data", "repos"),
          tmp: path.join(state, "tmp"),
        }),
      ),
      Effect.provide(FileSystem.layerNoop({})),
    ),
  )
  await ready.promise
  const capture = () => {
    if (setup.renderer.isDestroyed) throw new Error("the TUI has exited")
    return terminal.screen()
  }
  return {
    capture,
    links: () => terminal.links(),
    render: async () => {},
    async match(test) {
      for (let i = 0; i < 40; i++) {
        if (test(capture())) return true
        await Bun.sleep(25)
      }
      return false
    },
    type: async (text) => {
      for (const char of text) input(terminal.key(char === "\n" ? "enter" : char))
    },
    press: async (key, modifiers) => input(terminal.key(key, modifiers)),
    paste: async (text) => input(terminal.paste(text)),
    async click(x, y) {
      input(terminal.mouse("down", x, y, 0))
      await Bun.sleep(10)
      input(terminal.mouse("up", x, y, 0))
    },
    scroll: async (x, y, direction) => input(terminal.mouse("scroll", x, y, WHEEL[direction])),
    async resize(width, height) {
      terminal.resize(width, height)
      Object.assign(stdout, { columns: width, rows: height })
      setup.resize(width, height)
    },
    async close() {
      if (!setup.renderer.isDestroyed) setup.renderer.destroy()
      await task.catch(() => undefined)
      process.chdir(previous)
      terminal.free()
      if (raw) saveRaw(raw, ctx, Buffer.concat(recorded), sent)
    },
  }
}

async function terminalScreen(
  ctx: Ctx,
  url: string,
  args: Record<string, unknown>,
  config: Record<string, unknown>,
  size: { width: number; height: number },
): Promise<Screen> {
  const flags: string[] = []
  for (const [key, value] of Object.entries(args)) {
    if (value === undefined || value === false) continue
    if (key === "continue") flags.push("--continue")
    else if (key === "sessionID" || key === "newSessionID") flags.push("--session", String(value))
    else if (key === "prompt") flags.push("--prompt", String(value))
    else if (key === "auto") flags.push("--auto")
    else throw new Error(`TUI argument "${key}" has no CLI flag: mark the scenario inProcessOnly`)
  }
  const state = path.join(process.env.CONFORMANCE_ROOT!, "tui", path.basename(ctx.project))
  for (const dir of ["config/opencode", "data", "state", "cache", "tmp"])
    mkdirSync(path.join(state, dir), { recursive: true })
  writeFileSync(path.join(state, "config", "opencode", "cli.json"), JSON.stringify(config, null, 2) + "\n")
  const network = await fakeNetwork(ctx.provider.fetch, { direct: ["127.0.0.1", "localhost", "::1"] })
  const env: Record<string, string> = {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    HOME: process.env.HOME!,
    XDG_CONFIG_HOME: path.join(state, "config"),
    XDG_DATA_HOME: path.join(state, "data"),
    XDG_STATE_HOME: path.join(state, "state"),
    XDG_CACHE_HOME: path.join(state, "cache"),
    TMPDIR: path.join(state, "tmp"),
    TZ: "UTC",
    LANG: "C.UTF-8",
    TERM: "xterm-256color",
    SHELL: "/bin/bash",
    OPENCODE_DISABLE_AUTOUPDATE: "1",
    ...network.env,
    ...(runsTs() ? tsEnv() : {}),
  }
  const command = [...opencodeCommand(), "--server", url, ...flags]
  const raw = process.env.CONFORMANCE_TUI_RAW
  const pty = await PtyTerminal.open(command, {
    cwd: ctx.project,
    env,
    cols: size.width,
    rows: size.height,
    record: raw !== undefined,
  })
  const gone = () => pty.exited !== null
  // Started once something is on the screen.
  const deadline = Date.now() + 30_000
  while (!pty.terminal.screen().trim()) {
    if (gone() || Date.now() > deadline) {
      const output = pty.output()
      await pty.close()
      network.stop()
      throw new Error(`the TUI did not start (exit ${pty.exited}):\n${output.slice(-4000)}`)
    }
    await Bun.sleep(10)
  }
  const capture = () => {
    if (gone()) throw new Error("the TUI has exited")
    return pty.terminal.screen()
  }
  return {
    capture,
    links: () => pty.terminal.links(),
    render: async () => {},
    async match(test) {
      // About as long as a test renderer's frame budget.
      for (let i = 0; i < 40; i++) {
        if (test(capture())) return true
        await Bun.sleep(25)
      }
      return false
    },
    type: async (text) => {
      for (const char of text) pty.send(pty.terminal.key(char === "\n" ? "enter" : char))
    },
    press: async (key, modifiers) => pty.send(pty.terminal.key(key, modifiers)),
    paste: async (text) => pty.send(pty.terminal.paste(text)),
    async click(x, y) {
      pty.send(pty.terminal.mouse("down", x, y, 0))
      await Bun.sleep(10)
      pty.send(pty.terminal.mouse("up", x, y, 0))
    },
    scroll: async (x, y, direction) => pty.send(pty.terminal.mouse("scroll", x, y, WHEEL[direction])),
    resize: async (width, height) => pty.resize(width, height),
    async close() {
      const memory = gone() ? undefined : status(pty.pid)
      await pty.close()
      network.stop()
      if (raw) saveRaw(raw, ctx, pty.raw(), pty.inputs())
      // The TUI's peak resident memory, next to the server's (lib/server.ts).
      const log = process.env.CONFORMANCE_MEMORY_LOG
      const impl = implementation()
      if (log && memory && impl.kind === "process")
        appendFileSync(
          log,
          JSON.stringify({ impl: impl.name, role: "tui", ...ctx.label, peakKiB: memory.peak, rssKiB: memory.rss }) +
            "\n",
        )
    },
  }
}

/**
 * CONFORMANCE_TUI_RAW=<dir>: everything the TUI wrote to its terminal and every input it was sent, one pair of files
 * per scenario (debugging).
 */
function saveRaw(dir: string, ctx: Ctx, output: Uint8Array, inputs: readonly string[]) {
  const file = path.join(dir, ctx.label.file.replace(/\.test\.ts$/, ""), ctx.label.scenario.replace(/[^\w.-]+/g, "-"))
  mkdirSync(path.dirname(file), { recursive: true })
  writeFileSync(file + ".raw", output)
  writeFileSync(file + ".input.json", JSON.stringify(inputs, null, 1))
}

/** How long an input may take to change the screen before the TUI counts as having reacted to it (ms). */
const REACTION = 300

/** SGR mouse buttons of the wheel. */
const WHEEL = { up: 64, down: 65, left: 66, right: 67 } as const

// Equality rules for frames:
// - step timing and throughput in status lines ("· 110ms · 204.1 tok/s", "Thought: 52ms") are timing;
// - the prompt's example text ("Ask anything… "Fix broken tests"") is picked at random from a fixed list.
function untimed(line: string) {
  return line
    .replace(/(…\s*)"[^"]*"/g, '$1"<placeholder>"')
    .replace(/(·\s+|Thought:?\s+)(\d+h\s*)?(\d+m\s*)?\d+(\.\d+)?(ms|s)\b/g, "$1<duration>")
    .replace(/\d+(\.\d+)?\s*tok\/s/g, "<rate> tok/s")
}
