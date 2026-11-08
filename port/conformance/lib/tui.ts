// TUI driver. Renders the real opencode TUI (packages/tui) in a headless test terminal, connected over HTTP to
// the scenario's server exactly as in production, drives it with keystrokes, and records screen frames into
// the scenario's transcript. Server-side effects (provider requests, events, files) are recorded as usual.
//
// Run TUI scenarios from port/conformance/tui (its bunfig.toml adds the SolidJS JSX transform).
import { createTestRenderer } from "@opentui/core/testing"
import { Effect, FileSystem } from "effect"
import path from "node:path"
import { mkdirSync } from "node:fs"
import { Global } from "@opencode/util/global"
import { waitForHighlightedFrame } from "../ref/packages/tui/test/fixture/frame"
import { scenario, type Ctx, type ScenarioOptions } from "./scenario"

export interface TuiOptions extends ScenarioOptions {
  readonly width?: number
  readonly height?: number
  /** TUI command-line arguments (model, agent, prompt, continue, sessionID). */
  readonly args?: Record<string, unknown>
  /** TUI config (tui.json equivalent). Animations are always off: they are timing, not behavior. */
  readonly tui?: Record<string, unknown>
  /**
   * Runs against the server before the TUI starts (e.g. create sessions to open with --session/--continue).
   * Whatever it returns is merged into `args`.
   */
  readonly setup?: (ctx: Ctx) => Promise<Record<string, unknown> | void>
  /** Record which endpoints the TUI called (default). Off only where that set races in the TUI itself. */
  readonly requests?: boolean
}

type Key = Parameters<Awaited<ReturnType<typeof createTestRenderer>>["mockInput"]["pressKey"]>

export interface TuiCtx extends Ctx {
  type(text: string): Promise<void>
  /**
   * Press a key: a single character, an opentui KeyCodes name (RETURN, ARROW_UP...), a raw sequence, or a
   * name: enter, tab, backspace, delete, escape, space, up, down, left, right, home, end, pageup, pagedown, f1-f12.
   */
  press(...key: Key): Promise<void>
  enter(): Promise<void>
  escape(): Promise<void>
  tab(): Promise<void>
  arrow(direction: "up" | "down" | "left" | "right"): Promise<void>
  paste(text: string): Promise<void>
  /** Wait until the screen contains the text (or matches), then return the frame. */
  waitFor(match: string | RegExp, timeout?: number): Promise<string>
  /** Capture the screen once it has stopped changing and record it under a label. */
  frame(label: string): Promise<string>
  resize(width: number, height: number): Promise<void>
  click(x: number, y: number): Promise<void>
  scroll(x: number, y: number, direction: "up" | "down" | "left" | "right"): Promise<void>
  /** The raw opentui mock mouse (drag, double-click, right button...); follow it with `frame`/`waitFor`. */
  readonly mouse: Awaited<ReturnType<typeof createTestRenderer>>["mockMouse"]
}

const NAMED_KEYS: Record<string, string> = {
  enter: "\r",
  return: "\r",
  tab: "\t",
  backspace: "\b",
  delete: "\x1b[3~",
  escape: "\x1b",
  esc: "\x1b",
  space: " ",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
  home: "\x1b[H",
  end: "\x1b[F",
  pageup: "\x1b[5~",
  pagedown: "\x1b[6~",
  ...Object.fromEntries(
    [
      "\x1bOP",
      "\x1bOQ",
      "\x1bOR",
      "\x1bOS",
      "\x1b[15~",
      "\x1b[17~",
      "\x1b[18~",
      "\x1b[19~",
      "\x1b[20~",
      "\x1b[21~",
      "\x1b[23~",
      "\x1b[24~",
    ].map((sequence, index) => [`f${index + 1}`, sequence]),
  ),
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
    const setup = await createTestRenderer({
      width: options.width ?? 100,
      height: options.height ?? 30,
      kittyKeyboard: true,
    })
    await setup.renderer.setupTerminal()
    setup.renderer.start()
    const { run } = await import("../ref/packages/tui/src/app")
    const state = path.join(process.env.CONFORMANCE_ROOT!, "tui", path.basename(ctx.project))
    for (const dir of ["home", "data", "cache", "config", "state", "tmp"])
      mkdirSync(path.join(state, dir), { recursive: true })
    let config: Record<string, unknown> = structuredClone({ ...options.tui, animations: false })
    const ready = Promise.withResolvers<void>()
    const args = { ...options.args, ...((options.setup ? await options.setup(ctx) : undefined) ?? {}) }
    const previous = process.cwd()
    // The TUI opens the project it is started in.
    process.chdir(ctx.project)
    const task = Effect.runPromise(
      run({
        app: { name: "conformance", version: "conformance", channel: "conformance" },
        server: { endpoint: { url: server.url.toString() } },
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
        terminalHandoff: async () => ({ renderer: setup.renderer, mode: "dark", complete: ready.resolve }),
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
    const stable = async () => {
      let last = ""
      let same = 0
      // Five identical captures (~75 ms): asynchronous rendering (markdown, a reloaded transcript) can pause longer
      // than a shorter window under load.
      for (let i = 0; i < 200 && same < 5; i++) {
        await setup.renderOnce()
        await new Promise((resolve) => setTimeout(resolve, 15))
        const frame = setup.captureCharFrame()
        same = frame === last ? same + 1 : 0
        last = frame
      }
      return last
    }
    const after = async (action: () => void | Promise<void>) => {
      await action()
      await stable()
    }
    const t: TuiCtx = {
      ...ctx,
      type: (text) => after(() => setup.mockInput.typeText(text)),
      press: (key, ...rest) =>
        after(() =>
          setup.mockInput.pressKey(
            ...([typeof key === "string" ? (NAMED_KEYS[key.toLowerCase()] ?? key) : key, ...rest] as Key),
          ),
        ),
      enter: () => after(() => setup.mockInput.pressEnter()),
      escape: () => after(() => setup.mockInput.pressEscape()),
      tab: () => after(() => setup.mockInput.pressTab()),
      arrow: (direction) => after(() => setup.mockInput.pressArrow(direction)),
      paste: (text) => after(() => setup.mockInput.pasteBracketedText(text)),
      async waitFor(match, timeout = 20_000) {
        const test = (frame: string) => (typeof match === "string" ? frame.includes(match) : match.test(frame))
        // opentui's waitForFrame gives up after a fixed number of passes; retry until the timeout.
        const deadline = Date.now() + timeout
        while (true) {
          const found = await waitForHighlightedFrame(setup, test).then(
            () => true,
            () => false,
          )
          if (found) return stable()
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
        return frame
      },
      resize: (width, height) => after(() => setup.resize(width, height)),
      click: (x, y) => after(() => setup.mockMouse.click(x, y)),
      scroll: (x, y, direction) => after(() => setup.mockMouse.scroll(x, y, direction)),
      mouse: setup.mockMouse,
    }
    try {
      await ready.promise
      await stable()
      await body(t)
    } finally {
      if (!setup.renderer.isDestroyed) setup.renderer.destroy()
      await task.catch(() => undefined)
      await server.stop(true)
      process.chdir(previous)
    }
    // Which endpoints the TUI writes to is part of its contract with the server; concurrent order is not. Reads
    // are not recorded: the client's data layer refetches on bursts of events and reconciles events that
    // overtake responses, so which reads it makes is timing (and the frames show what it loaded).
    if (options.requests !== false)
      ctx.note("tui-requests", [...new Set(requests.filter((request) => !request.startsWith("GET ")))].sort())
  })
}

// Equality rules for frames:
// - step timing and throughput in status lines ("· 110ms · 204.1 tok/s", "Thought: 52ms") are timing;
// - the prompt's example text ("Ask anything… "Fix broken tests"") is picked at random from a fixed list.
function untimed(line: string) {
  return line
    .replace(/(…\s*)"[^"]*"/g, '$1"<placeholder>"')
    .replace(/(·\s+|Thought:?\s+)(\d+h\s*)?(\d+m\s*)?\d+(\.\d+)?(ms|s)\b/g, "$1<duration>")
    .replace(/\d+(\.\d+)?\s*tok\/s/g, "<rate> tok/s")
}
