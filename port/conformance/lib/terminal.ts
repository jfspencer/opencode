// A real terminal for driving a TUI: Ghostty's terminal core (libghostty-vt compiled to WASM, from the ghostty-web
// package the reference already depends on). An application attached to it sees an ordinary interactive terminal:
// it can query it (device attributes, modes, the kitty keyboard flags, the color scheme) and gets Ghostty's answers;
// keys reach it encoded by Ghostty's key encoder for whatever keyboard protocol it turned on; the screen is read
// back from Ghostty's grid as text. VirtualTerminal is the terminal; PtyTerminal attaches a process to one through a
// pseudo-terminal (Bun.spawn's `terminal`).
//
// Nothing here knows about opencode: it runs an application in a terminal and reports what is on the screen.
import path from "node:path"
import { REPO } from "./server"

/** Ghostty key codes (ghostty-web lib/types.ts `Key`; libghostty-vt's GhosttyKey). */
const KEY = {
  GRAVE: 1,
  BACKSLASH: 2,
  BRACKET_LEFT: 3,
  BRACKET_RIGHT: 4,
  COMMA: 5,
  ZERO: 6,
  EQUAL: 16,
  A: 20,
  MINUS: 46,
  PERIOD: 47,
  QUOTE: 48,
  SEMICOLON: 49,
  SLASH: 50,
  BACKSPACE: 53,
  ENTER: 58,
  SPACE: 63,
  TAB: 64,
  DELETE: 68,
  END: 69,
  HOME: 71,
  INSERT: 72,
  PAGE_DOWN: 73,
  PAGE_UP: 74,
  DOWN: 75,
  LEFT: 76,
  RIGHT: 77,
  UP: 78,
  ESCAPE: 120,
  F1: 121,
} as const

const MOD = { SHIFT: 1, CTRL: 2, ALT: 4, SUPER: 8 } as const

export interface Modifiers {
  readonly shift?: boolean
  readonly ctrl?: boolean
  /** Alt/Option (the terminal "meta" key). */
  readonly meta?: boolean
  readonly super?: boolean
}

/** A US keyboard: the physical key for a shifted or unshifted character, and the key's unshifted character. */
const PUNCTUATION: Record<string, [key: number, base: string]> = {
  "`": [KEY.GRAVE, "`"],
  "~": [KEY.GRAVE, "`"],
  "\\": [KEY.BACKSLASH, "\\"],
  "|": [KEY.BACKSLASH, "\\"],
  "[": [KEY.BRACKET_LEFT, "["],
  "{": [KEY.BRACKET_LEFT, "["],
  "]": [KEY.BRACKET_RIGHT, "]"],
  "}": [KEY.BRACKET_RIGHT, "]"],
  ",": [KEY.COMMA, ","],
  "<": [KEY.COMMA, ","],
  "=": [KEY.EQUAL, "="],
  "+": [KEY.EQUAL, "="],
  "-": [KEY.MINUS, "-"],
  _: [KEY.MINUS, "-"],
  ".": [KEY.PERIOD, "."],
  ">": [KEY.PERIOD, "."],
  "'": [KEY.QUOTE, "'"],
  '"': [KEY.QUOTE, "'"],
  ";": [KEY.SEMICOLON, ";"],
  ":": [KEY.SEMICOLON, ";"],
  "/": [KEY.SLASH, "/"],
  "?": [KEY.SLASH, "/"],
  " ": [KEY.SPACE, " "],
  ...Object.fromEntries([..."!@#$%^&*()"].map((char, index) => [char, [KEY.ZERO + ((index + 1) % 10), String((index + 1) % 10)]])),
}

/** Named keys (lowercase): what a key press is called in scenarios, and the raw sequences scenarios may use. */
const NAMED: Record<string, number> = {
  return: KEY.ENTER,
  enter: KEY.ENTER,
  "\r": KEY.ENTER,
  linefeed: KEY.ENTER,
  tab: KEY.TAB,
  "\t": KEY.TAB,
  backspace: KEY.BACKSPACE,
  "\b": KEY.BACKSPACE,
  "\x7f": KEY.BACKSPACE,
  delete: KEY.DELETE,
  "\x1b[3~": KEY.DELETE,
  insert: KEY.INSERT,
  escape: KEY.ESCAPE,
  esc: KEY.ESCAPE,
  "\x1b": KEY.ESCAPE,
  space: KEY.SPACE,
  up: KEY.UP,
  arrow_up: KEY.UP,
  "\x1b[a": KEY.UP,
  down: KEY.DOWN,
  arrow_down: KEY.DOWN,
  "\x1b[b": KEY.DOWN,
  right: KEY.RIGHT,
  arrow_right: KEY.RIGHT,
  "\x1b[c": KEY.RIGHT,
  left: KEY.LEFT,
  arrow_left: KEY.LEFT,
  "\x1b[d": KEY.LEFT,
  home: KEY.HOME,
  "\x1b[h": KEY.HOME,
  end: KEY.END,
  "\x1b[f": KEY.END,
  pageup: KEY.PAGE_UP,
  "\x1b[5~": KEY.PAGE_UP,
  pagedown: KEY.PAGE_DOWN,
  "\x1b[6~": KEY.PAGE_DOWN,
  ...Object.fromEntries(Array.from({ length: 12 }, (_, index) => [`f${index + 1}`, KEY.F1 + index])),
  ...Object.fromEntries(
    ["\x1bop", "\x1boq", "\x1bor", "\x1bos", "\x1b[15~", "\x1b[17~", "\x1b[18~", "\x1b[19~", "\x1b[20~", "\x1b[21~", "\x1b[23~", "\x1b[24~"].map(
      (sequence, index) => [sequence, KEY.F1 + index],
    ),
  ),
}

/** What legacy terminals send for keys Ghostty's encoder leaves to the host (as ghostty-web's input handler does). */
const HOST_DEFAULTS: Record<number, string> = { [KEY.BACKSPACE]: "\x7f" }

type Exports = Record<string, any> & { memory: WebAssembly.Memory }

interface Ghostty {
  createTerminal(cols: number, rows: number, config?: Record<string, number>): GhosttyTerminal
}
interface GhosttyCell {
  codepoint: number
  width: number
  grapheme_len: number
  hyperlink_id: number
}
interface GhosttyTerminal {
  readonly handle: number
  write(data: string | Uint8Array): void
  resize(cols: number, rows: number): void
  setColorScheme(scheme: "dark" | "light"): void
  update(): number
  getLine(y: number): GhosttyCell[] | null
  getGraphemeString(row: number, col: number): string
  getHyperlinkUri(row: number, col: number): string | null
  getMode(mode: number, isAnsi?: boolean): boolean
  isAlternateScreen(): boolean
  hasMouseTracking(): boolean
  hasBracketedPaste(): boolean
  hasResponse(): boolean
  readResponse(): string | null
  getDimensions(): { cols: number; rows: number }
  free(): void
}

let loaded: Promise<{ ghostty: Ghostty; exports: Exports }> | undefined
function load() {
  loaded ??= (async () => {
    const entry = Bun.resolveSync("ghostty-web", path.join(REPO, "packages/app"))
    const root = path.resolve(path.dirname(entry), "..")
    const { Ghostty } = await import(entry)
    const ghostty = await Ghostty.load(path.join(root, "ghostty-vt.wasm"))
    const exports = (ghostty as { exports: Exports }).exports
    const self = { ghostty: ghostty as Ghostty, exports }
    // Self-check of the one private detail used below (where ghostty-web's wrapper keeps the core terminal): a
    // terminal that turned on the kitty keyboard protocol must get ctrl+x in that protocol.
    const probe = ghostty.createTerminal(10, 2) as GhosttyTerminal
    probe.write("\x1b[>1u")
    const keys = new KeyEncoder(self, probe)
    const encoded = keys.encode(KEY.A + 23, MOD.CTRL, "", 0x78)
    keys.dispose()
    probe.free()
    if (encoded !== "\x1b[120;5u")
      throw new Error(`ghostty-web's terminal layout changed (ctrl+x encoded as ${JSON.stringify(encoded)})`)
    return self
  })()
  return loaded
}

// ghostty-web's terminal handle is its own wrapper struct; libghostty-vt's encoders take the core terminal it holds.
// The offset is that field's place in the wrapper of the pinned build (checked on load).
const CORE_TERMINAL_OFFSET = 1208

/** Ghostty's key encoder, configured from the terminal's state (cursor key mode, kitty flags, alt prefix...). */
class KeyEncoder {
  private readonly exports: Exports
  private readonly encoder: number
  private readonly core: number
  constructor(self: { exports: Exports }, terminal: GhosttyTerminal) {
    this.exports = self.exports
    const memory = () => new DataView(this.exports.memory.buffer)
    const slot = this.exports.ghostty_wasm_alloc_opaque()
    if (this.exports.ghostty_key_encoder_new(0, slot) !== 0) throw new Error("ghostty: key encoder allocation failed")
    this.encoder = memory().getUint32(slot, true)
    this.exports.ghostty_wasm_free_opaque(slot)
    this.core = memory().getUint32(terminal.handle + CORE_TERMINAL_OFFSET, true)
  }

  encode(key: number, mods: number, utf8: string, unshifted: number, consumed = 0): string {
    const ex = this.exports
    const memory = () => new DataView(ex.memory.buffer)
    ex.ghostty_key_encoder_setopt_from_terminal(this.encoder, this.core)
    const slot = ex.ghostty_wasm_alloc_opaque()
    if (ex.ghostty_key_event_new(0, slot) !== 0) throw new Error("ghostty: key event allocation failed")
    const event = memory().getUint32(slot, true)
    ex.ghostty_wasm_free_opaque(slot)
    const text = new TextEncoder().encode(utf8)
    const textPointer = text.length ? ex.ghostty_wasm_alloc_u8_array(text.length) : 0
    const output = ex.ghostty_wasm_alloc_u8_array(128)
    const written = ex.ghostty_wasm_alloc_usize()
    try {
      ex.ghostty_key_event_set_action(event, 1)
      ex.ghostty_key_event_set_key(event, key)
      ex.ghostty_key_event_set_mods(event, mods)
      ex.ghostty_key_event_set_consumed_mods(event, consumed)
      if (text.length) {
        new Uint8Array(ex.memory.buffer).set(text, textPointer)
        ex.ghostty_key_event_set_utf8(event, textPointer, text.length)
      }
      if (unshifted) ex.ghostty_key_event_set_unshifted_codepoint(event, unshifted)
      if (ex.ghostty_key_encoder_encode(this.encoder, event, output, 128, written) !== 0)
        throw new Error("ghostty: key encoding failed")
      const bytes = new Uint8Array(ex.memory.buffer, output, memory().getUint32(written, true)).slice()
      return new TextDecoder().decode(bytes)
    } finally {
      if (textPointer) ex.ghostty_wasm_free_u8_array(textPointer, text.length)
      ex.ghostty_wasm_free_u8_array(output, 128)
      ex.ghostty_wasm_free_usize(written)
      ex.ghostty_key_event_free(event)
    }
  }

  dispose() {
    this.exports.ghostty_key_encoder_free(this.encoder)
  }
}

/**
 * A terminal without a process: Ghostty's terminal core. What an application writes goes in through `write`; the
 * terminal's answers to its queries come out through `respond`; keys, pastes and mouse events are encoded for the
 * modes the application turned on; the screen is read back as text. A real process attaches through PtyTerminal;
 * the in-process TUI attaches its renderer's output and input streams directly (lib/tui.ts).
 */
export class VirtualTerminal {
  private constructor(
    private readonly term: GhosttyTerminal,
    private readonly keys: KeyEncoder,
    private readonly respond: (data: string) => void,
  ) {}

  static async create(cols: number, rows: number, respond: (data: string) => void): Promise<VirtualTerminal> {
    const self = await load()
    const term = self.ghostty.createTerminal(cols, rows, { fgColor: 0xffffff, bgColor: 0x000000 })
    // Answer color scheme queries (and report changes) as a dark terminal.
    term.setColorScheme("dark")
    return new VirtualTerminal(term, new KeyEncoder(self, term), respond)
  }

  /** Output from the application. */
  write(data: string | Uint8Array) {
    this.term.write(data)
    // The terminal's answers to the application's queries go back as input.
    while (this.term.hasResponse()) {
      const response = this.term.readResponse()
      if (response) this.respond(response)
    }
  }

  /** The visible screen as text: every row followed by a line break, wide characters once, padding kept. */
  screen(): string {
    this.term.update()
    const { cols, rows } = this.term.getDimensions()
    return Array.from({ length: rows }, (_, y) => {
      const cells = this.term.getLine(y) ?? []
      return (
        cells
          .slice(0, cols)
          .map((cell, x) =>
            cell.width === 0
              ? ""
              : cell.grapheme_len
                ? this.term.getGraphemeString(y, x)
                : cell.codepoint
                  ? String.fromCodePoint(cell.codepoint)
                  : " ",
          )
          .join("") + "\n"
      )
    }).join("")
  }

  /**
   * The hyperlinks (OSC 8) on the screen in reading order, `text -> uri`: one per run of cells with the same
   * target. A link the application wrapped (the last thing on a row, continued as the first thing on the next) is
   * one entry.
   */
  links(): string[] {
    this.term.update()
    const { cols, rows } = this.term.getDimensions()
    const links: { uri: string; text: string }[] = []
    let open: { uri: string; text: string } | undefined
    for (let y = 0; y < rows; y++) {
      const cells = (this.term.getLine(y) ?? []).slice(0, cols)
      // Runs of cells with the same target (null: no link), blank runs without a link dropped.
      const segments: { uri: string | null; text: string }[] = []
      cells.forEach((cell, x) => {
        if (cell.width === 0) return
        const uri = cell.hyperlink_id ? this.term.getHyperlinkUri(y, x) : null
        const char = cell.grapheme_len
          ? this.term.getGraphemeString(y, x)
          : cell.codepoint
            ? String.fromCodePoint(cell.codepoint)
            : " "
        const last = segments.at(-1)
        if (last && last.uri === uri) last.text += char
        else segments.push({ uri, text: char })
      })
      const visible = segments.filter((segment) => segment.uri !== null || segment.text.trim() !== "")
      visible.forEach((segment, index) => {
        if (segment.uri === null) return
        if (index === 0 && open?.uri === segment.uri) open.text += segment.text.trim()
        else links.push({ uri: segment.uri, text: segment.text.trim() })
      })
      const tail = visible.at(-1)
      open = tail?.uri ? links.at(-1) : undefined
    }
    return links.map((link) => `${link.text} -> ${link.uri}`)
  }

  get alternateScreen() {
    return this.term.isAlternateScreen()
  }

  /**
   * The input a key press sends: a printable character, a key name (enter, tab, backspace, delete, escape, space,
   * up/arrow_up..., home, end, pageup, pagedown, f1-f12; case-insensitive) or the legacy sequence of one of those.
   */
  key(name: string, modifiers: Modifiers = {}): string {
    const held =
      (modifiers.shift ? MOD.SHIFT : 0) |
      (modifiers.ctrl ? MOD.CTRL : 0) |
      (modifiers.meta ? MOD.ALT : 0) |
      (modifiers.super ? MOD.SUPER : 0)
    const named = NAMED[name.toLowerCase()]
    if (named !== undefined) {
      const encoded = this.keys.encode(named, held, named === KEY.SPACE ? " " : "", named === KEY.SPACE ? 0x20 : 0)
      return encoded || ((held & ~MOD.SHIFT) === 0 ? (HOST_DEFAULTS[named] ?? "") : "")
    }
    const chars = [...name]
    if (chars.length !== 1) throw new Error(`unknown key ${JSON.stringify(name)}`)
    const char = chars[0]
    const [key, base] = physicalKey(char)
    const mods = held | (base !== char && char.toLowerCase() !== base ? MOD.SHIFT : 0) | (/^[A-Z]$/.test(char) ? MOD.SHIFT : 0)
    // The text a key produces is the shifted character; with ctrl, alt or super held it produces none of its own.
    const text =
      mods & (MOD.CTRL | MOD.ALT | MOD.SUPER) ? "" : modifiers.shift && /^[a-z]$/.test(char) ? char.toUpperCase() : char
    const consumed = mods & MOD.SHIFT && text !== "" && text !== base ? MOD.SHIFT : 0
    return this.keys.encode(key, mods, text, base.codePointAt(0)!, consumed) || text
  }

  /** A paste: bracketed when the application asked for bracketed paste. */
  paste(text: string): string {
    return this.term.hasBracketedPaste() ? `\x1b[200~${text}\x1b[201~` : text.replace(/\r?\n/g, "\r")
  }

  /**
   * A mouse event at a cell (0-based), reported the way the application asked for (SGR when it enabled mode 1006),
   * or nothing when it doesn't track the mouse, as in a real terminal.
   */
  mouse(kind: "down" | "up" | "scroll", x: number, y: number, button: number): string {
    if (!this.term.hasMouseTracking()) return ""
    if (this.term.getMode(1006)) return `\x1b[<${button};${x + 1};${y + 1}${kind === "up" ? "m" : "M"}`
    return `\x1b[M${String.fromCharCode(32 + (kind === "up" ? 3 : button), 33 + x, 33 + y)}`
  }

  resize(cols: number, rows: number) {
    this.term.resize(cols, rows)
  }

  free() {
    this.keys.dispose()
    this.term.free()
  }
}

/** A US keyboard: the physical key for a printable character and its unshifted character (0: none). */
function physicalKey(char: string): [key: number, base: string] {
  if (/^[a-z]$/.test(char)) return [KEY.A + char.charCodeAt(0) - 97, char]
  if (/^[A-Z]$/.test(char)) return [KEY.A + char.charCodeAt(0) - 65, char.toLowerCase()]
  if (/^[0-9]$/.test(char)) return [KEY.ZERO + Number(char), char]
  return PUNCTUATION[char] ?? [0, char]
}

export interface TerminalOptions {
  readonly cwd: string
  readonly env: Record<string, string>
  readonly cols: number
  readonly rows: number
  /** Keep everything the process writes and is sent (for `raw()` and `inputs()`). */
  readonly record?: boolean
}

/** A process in a pseudo-terminal, attached to a VirtualTerminal. */
export class PtyTerminal {
  private exitCode: number | null = null
  private tail = ""
  private recorded: Uint8Array[] | undefined
  private sent: string[] | undefined
  private constructor(
    readonly terminal: VirtualTerminal,
    private readonly child: Bun.Subprocess,
  ) {
    void child.exited.then((code) => (this.exitCode = code))
  }

  static async open(command: readonly string[], options: TerminalOptions): Promise<PtyTerminal> {
    let instance: PtyTerminal | undefined
    const pending: Uint8Array[] = []
    const responses: string[] = []
    const terminal = await VirtualTerminal.create(options.cols, options.rows, (response) => {
      const pty = instance?.child.terminal
      if (pty && !pty.closed) pty.write(response)
      else responses.push(response)
    })
    const child = Bun.spawn([...command], {
      cwd: options.cwd,
      env: options.env,
      terminal: {
        cols: options.cols,
        rows: options.rows,
        name: options.env.TERM ?? "xterm-256color",
        data(_, data) {
          if (!instance) return void pending.push(data)
          instance.receive(data)
        },
      },
    })
    instance = new PtyTerminal(terminal, child)
    if (options.record) {
      instance.recorded = []
      instance.sent = []
    }
    for (const data of pending.splice(0)) instance.receive(data)
    for (const response of responses.splice(0)) child.terminal?.write(response)
    return instance
  }

  private receive(data: Uint8Array) {
    this.recorded?.push(data.slice())
    this.tail = (this.tail + new TextDecoder().decode(data)).slice(-16_000)
    this.terminal.write(data)
  }

  /** The process's exit code once it has exited. */
  get exited(): number | null {
    return this.exitCode
  }

  get pid() {
    return this.child.pid
  }

  /** Everything the process wrote, when opened with `record`. */
  raw(): Uint8Array {
    return Buffer.concat(this.recorded ?? [])
  }

  /** Every input sent to the process (keys, pastes, mouse reports), when opened with `record`. */
  inputs(): readonly string[] {
    return this.sent ?? []
  }

  /** The last output the process wrote (for diagnostics). */
  output() {
    return this.tail
  }

  /** Input to the process, as typed into the terminal. */
  send(data: string) {
    if (!data) return
    const pty = this.child.terminal
    if (!pty || pty.closed || this.exitCode !== null) throw new Error("the terminal's process has exited")
    this.sent?.push(data)
    pty.write(data)
  }

  resize(cols: number, rows: number) {
    this.terminal.resize(cols, rows)
    this.child.terminal?.resize(cols, rows)
  }

  /**
   * Close the terminal (the process gets SIGHUP, as when a terminal window closes), then end whatever is left of
   * its process group.
   */
  async close() {
    this.child.terminal?.close()
    await Promise.race([this.child.exited, Bun.sleep(3_000)])
    try {
      process.kill(-this.child.pid, "SIGKILL")
    } catch {}
    try {
      this.child.kill("SIGKILL")
    } catch {}
    this.terminal.free()
  }
}
