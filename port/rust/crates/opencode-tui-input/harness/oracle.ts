// The TS side of the opencode-tui-input differential check (stdin parsing). Generates programs and runs them,
// as they are generated, through the reference's own @opentui/core 0.8.1: a StdinParser made as the renderer makes
// it, on a manual clock, fed random terminal byte streams (keys in every encoding, Kitty, modifyOtherKeys, SGR and
// X10 mouse, bracketed paste, focus, terminal replies, junk) cut at random points, with time passing between the
// reads so the ESC timeout fires or not, and the protocol context changing as the renderer changes it. Also direct
// calls of parseKeypress, MouseParser, stripAnsiSequences and decodePasteBytes. Writes the programs and the answers;
// examples/input-driver.rs replays the programs through the Rust crate.
//
// usage: bun harness/oracle.ts --programs <file> --results <file> [--count N] [--seed S]
import { writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { handPicked } from "./cases"

const argv = process.argv.slice(2)
const arg = (name: string, fallback?: string) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : fallback
}
const here = dirname(fileURLToPath(import.meta.url))
const reference = join(here, "../../../../.reference")
const core = await import(Bun.resolveSync("@opentui/core", join(reference, "packages/tui/src")))
const { StdinParser, parseKeypress, MouseParser, stripAnsiSequences, decodePasteBytes } = core

// ---- formatting shared with the driver
const f64 = new DataView(new ArrayBuffer(8))
const num = (x: number) => {
  f64.setFloat64(0, x, true)
  return f64.getBigUint64(0, true).toString(16).padStart(16, "0")
}
const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString("hex") || "-"
const unhex = (text: string) => (text === "-" ? new Uint8Array(0) : Uint8Array.from(Buffer.from(text, "hex")))
/** Code points outside printable ASCII (and `\`) as \u{..}; a lone surrogate as U+FFFD, as Rust holds it. */
const esc = (text: string) => {
  let out = ""
  for (const ch of text) {
    let code = ch.codePointAt(0)!
    if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd
    out += code >= 0x20 && code <= 0x7e && ch !== "\\" ? ch : `\\u{${code.toString(16)}}`
  }
  return out
}
const opt = (value: unknown) => (value === undefined ? "-" : typeof value === "boolean" ? (value ? "1" : "0") : String(value))

function key(k: any): string {
  return [
    `name=${esc(k.name)}`,
    `ctrl=${opt(k.ctrl)} meta=${opt(k.meta)} shift=${opt(k.shift)} option=${opt(k.option)} number=${opt(k.number)}`,
    `seq=${esc(k.sequence)}`,
    `raw=${esc(k.raw)}`,
    `type=${k.eventType} source=${k.source} code=${k.code === undefined ? "-" : esc(k.code)}`,
    `super=${opt(k.super)} hyper=${opt(k.hyper)} caps=${opt(k.capsLock)} num=${opt(k.numLock)}`,
    `base=${opt(k.baseCode)} repeated=${opt(k.repeated)}`,
  ].join(" ")
}

function mouse(m: any): string {
  const mods = m.modifiers
  const scroll = m.scroll ? `${m.scroll.direction}:${num(m.scroll.delta)}` : "-"
  return `type=${m.type} button=${m.button} x=${num(m.x)} y=${num(m.y)} mods=${Number(mods.shift)}${Number(mods.alt)}${Number(mods.ctrl)} scroll=${scroll}`
}

function event(e: any): string {
  switch (e.type) {
    case "key":
      return `E key raw=${esc(e.raw)} ${key(e.key)}`
    case "mouse":
      return `E mouse raw=${esc(e.raw)} ${e.encoding} ${mouse(e.event)}`
    case "paste":
      return `E paste ${hex(e.bytes)} meta=${e.metadata === undefined ? "-" : JSON.stringify(e.metadata)}`
    case "response":
      return `E resp ${e.protocol} ${esc(e.sequence)}`
  }
  throw new Error(`event ${e.type}`)
}

// ---- the manual clock the parser runs on (the renderer's clock in tests)
class ManualClock {
  now_ = 0
  private timers = new Map<number, { at: number; fn: () => void }>()
  private next = 1
  now() {
    return this.now_
  }
  setTimeout(fn: () => void, delay: number) {
    const id = this.next++
    this.timers.set(id, { at: this.now_ + delay, fn })
    return id
  }
  clearTimeout(id: number) {
    this.timers.delete(id)
  }
  setInterval(): number {
    throw new Error("no intervals")
  }
  clearInterval() {}
  /** Fires each timer due by `target` in deadline order, at its deadline. */
  advance(ms: number) {
    const target = this.now_ + ms
    while (true) {
      let best: [number, { at: number; fn: () => void }] | undefined
      for (const entry of this.timers) if (entry[1].at <= target && (!best || entry[1].at < best[1].at)) best = entry
      if (!best) break
      this.timers.delete(best[0])
      this.now_ = best[1].at
      best[1].fn()
    }
    this.now_ = target
  }
}

const CONTEXT_FIELDS = [
  "kittyKeyboardEnabled",
  "privateCapabilityRepliesActive",
  "pixelResolutionQueryActive",
  "explicitWidthCprActive",
  "startupCursorCprActive",
  "replySentinelActive",
] as const

class Program {
  ops: string[] = []
  out: string[] = []
  clock = new ManualClock()
  parser: any = null

  drain() {
    this.parser.drain((e: any) => this.out.push(event(e)))
  }

  guard(f: () => void) {
    try {
      f()
    } catch (error) {
      this.out.push(`err ${esc(error instanceof Error ? error.message : String(error))}`)
    }
  }

  exec(op: string) {
    this.ops.push(op)
    const [name, ...args] = op.split(" ")
    const out = this.out
    switch (name) {
      case "new": {
        // new <timeout|-> <max|-> <arm> <kitty|-> <context bits>
        const [timeout, max, arm, kitty, bits] = args
        const protocolContext: Record<string, boolean> = {}
        CONTEXT_FIELDS.forEach((field, i) => {
          if (bits[i] !== "-") protocolContext[field] = bits[i] === "1"
        })
        this.parser = new StdinParser({
          timeoutMs: timeout === "-" ? undefined : Number(timeout),
          maxPendingBytes: max === "-" ? undefined : Number(max),
          armTimeouts: arm === "1",
          onTimeoutFlush: () => {
            out.push(`T ${num(this.clock.now())}`)
            this.drain()
          },
          useKittyKeyboard: kitty === "-" ? undefined : kitty === "1",
          protocolContext,
          clock: this.clock,
        })
        out.push(`cap ${this.parser.bufferCapacity}`)
        return
      }
      case "push":
        return this.guard(() => {
          this.parser.push(unhex(args[0]))
          this.drain()
        })
      case "adv":
        this.clock.advance(Number(args[0]))
        out.push(`now ${num(this.clock.now())}`)
        return
      case "ctx":
        return this.guard(() => {
          this.parser.updateProtocolContext({ [CONTEXT_FIELDS[Number(args[0])]]: args[1] === "1" })
          if (args[2] === "1") this.drain()
        })
      case "abort":
        return this.guard(() => this.parser.abortPendingStartupCursorCpr())
      case "pause":
        return this.guard(() => this.parser.pausePendingTimeout())
      case "resume":
        return this.guard(() => this.parser.resumePendingTimeout())
      case "mreset":
        return this.guard(() => this.parser.resetMouseState())
      case "reset":
        return this.guard(() => this.parser.reset())
      case "destroy":
        return this.guard(() => this.parser.destroy())
      case "flush":
        return this.guard(() => {
          this.parser.flushTimeout(args[0] === "-" ? undefined : this.clock.now() + Number(args[0]))
          this.drain()
        })
      case "read":
        return this.guard(() => {
          const e = this.parser.read()
          out.push(e ? event(e) : "none")
        })
      case "pix":
        out.push(`pix ${Number(this.parser.hasPendingPixelResolutionResponse())}`)
        return
      case "cap":
        out.push(`cap ${this.parser.bufferCapacity}`)
        return
      // Direct calls.
      case "pk": {
        const text = new TextDecoder().decode(unhex(args[0]))
        const parsed = parseKeypress(text, { useKittyKeyboard: args[1] === "1" })
        out.push(parsed ? key(parsed) : "null")
        return
      }
      case "pkb": {
        const parsed = parseKeypress(Buffer.from([Number(args[0])]), { useKittyKeyboard: args[1] === "1" })
        out.push(parsed ? key(parsed) : "null")
        return
      }
      case "mall": {
        const parser = new MouseParser()
        for (const chunk of args) out.push(parser.parseAllMouseEvents(unhex(chunk)).map(mouse).join(" ; ") || "none")
        return
      }
      case "strip":
        out.push(esc(stripAnsiSequences(new TextDecoder().decode(unhex(args[0])))))
        return
      case "dec":
        out.push(esc(decodePasteBytes(unhex(args[0]))))
        return
    }
    throw new Error(`op ${op}`)
  }
}

// ---- generation
class Rng {
  private s: number
  constructor(seed: number) {
    this.s = seed >>> 0 || 1
  }
  next() {
    let t = (this.s = (this.s + 0x6d2b79f5) >>> 0)
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  int(lo: number, hi: number) {
    return lo + Math.floor(this.next() * (hi - lo + 1))
  }
  chance(p: number) {
    return this.next() < p
  }
  pick<T>(items: readonly T[]): T {
    return items[Math.floor(this.next() * items.length)]
  }
}

const enc = new TextEncoder()
const ascii = (text: string) => Uint8Array.from(text, (c) => c.charCodeAt(0))
const concat = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0))
  let offset = 0
  for (const part of parts) {
    out.set(part, offset)
    offset += part.length
  }
  return out
}

const LEGACY = [
  "[A", "[B", "[C", "[D", "[E", "[F", "[H", "[P", "[Q", "[R", "[S", "[Z", "[a", "[b", "[c", "[d", "[e",
  "[1~", "[2~", "[3~", "[4~", "[5~", "[6~", "[7~", "[8~", "[11~", "[12~", "[13~", "[14~", "[15~", "[17~", "[18~",
  "[19~", "[20~", "[21~", "[23~", "[24~", "[29~", "[57427~", "[2$", "[3$", "[5$", "[6$", "[7$", "[8$", "[9$",
  "[2^", "[3^", "[5^", "[6^", "[7^", "[8^", "[[A", "[[B", "[[C", "[[D", "[[E", "[[5~", "[[6~", "[[1~", "[200~",
  "[201~", "[I", "[O", "[3;5~", "[1;5A", "[1;2B", "[1;3C", "[1;9D", "[1;17H", "[1;6F", "[5;7~", "[2;33~",
  "[1;99999999999A", "[13;2u", "[1;1:2A",
  "OA", "OB", "OC", "OD", "OE", "OF", "OH", "OM", "OP", "OQ", "OR", "OS", "Oa", "Ob", "Oc", "Od", "Oe", "Oj", "Ok",
  "Ol", "Om", "On", "Oo", "Op", "Oq", "Or", "Os", "Ot", "Ou", "Ov", "Ow", "Ox", "Oy", "OX", "O5A", "O1;3P", "NA",
]
const REPLIES = [
  "\x1b[?62;22;52c", "\x1b[?1;2c", "\x1b[>1;10;0c", "\x1b[?2026;2$y", "\x1b[?2027;0$y", "\x1b[?1u", "\x1b[?31u",
  "\x1b[?0u", "\x1b[?997;1n", "\x1b[?997;2n", "\x1b[12;40R", "\x1b[1;2R", "\x1b[1;3R", "\x1b[4;600;800t",
  "\x1b[6;16;8t", "\x1b[8;24;80t", "\x1bP>|kitty(0.35.2)\x1b\\", "\x1bP1$r0m\x1b\\", "\x1bP0$r\x1b\\",
  "\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\", "\x1b]10;rgb:ffff/ffff/ffff\x07", "\x1b]4;1;rgb:cd/00/00\x07",
  "\x1b]52;c;aGVsbG8=\x07", "\x1b_Gi=31;OK\x1b\\", "\x1b_Gi=1;ENOENT:no such file\x1b\\", "\x1b[?u", "\x1b[0n",
  "\x1b[?1016;1$y", "\x1b[3;1R", "\x1b[4;1;2t", "\x1b[?62c", "\x1b]1337;ReportCellSize=17.0;8.0\x07",
]
const UTF8_CHARS = ["é", "ß", "中", "ж", "Ж", "ㅊ", "😀", "👍🏽", " ", "﻿", "á", "ǅ", "İ", "ﬀ", " "]

function kitty(rng: Rng): string {
  if (rng.chance(0.2)) {
    // functional and tilde keys with an event type
    const number = rng.chance(0.5) ? "1" : rng.pick(["2", "3", "5", "6", "11", "15", "24", "29", "57427", "9", "7"])
    return `\x1b[${number};${rng.int(1, 130)}:${rng.int(0, 4)}${rng.pick(["A", "B", "C", "D", "H", "F", "E", "P", "Q", "S", "R", "Z", "~"])}`
  }
  const code = rng.pick([
    () => String(rng.int(32, 126)),
    () => String(rng.pick([9, 13, 27, 127, 32, 0, 8])),
    () => String(rng.int(57344, 57454)),
    () => String(rng.int(0x80, 0x2fff)),
    () => String(rng.int(0x1f300, 0x1faff)),
    () => String(rng.pick([0xd800, 0xdfff, 0x10ffff, 0x110000, 99999999999])),
    () => rng.pick(["", "x", "-0", "+5", " 97", "0x61", "97.5"]),
  ])()
  const parts = [code]
  if (rng.chance(0.3)) parts.push(rng.chance(0.8) ? String(rng.int(0, 0x2fff)) : "")
  if (rng.chance(0.25)) {
    if (parts.length === 1) parts.push("")
    parts.push(String(rng.pick([99, 97, 0, 1114112, rng.int(1, 200)])))
  }
  let body = parts.join(":")
  if (rng.chance(0.6)) {
    const mods = rng.chance(0.9) ? String(rng.int(1, 256)) : rng.pick(["", "x", "1e3", "99999999999999"])
    body += `;${mods}`
    if (rng.chance(0.4)) body += `:${rng.pick(["1", "2", "3", "4", "", "x"])}`
  }
  if (rng.chance(0.25)) {
    if (!body.includes(";")) body += ";"
    const cps = Array.from({ length: rng.int(1, 3) }, () => String(rng.pick([rng.int(32, 0x2fff), 0, 0xd83d, 1114112, 65])))
    body += `;${cps.join(":")}`
  }
  return `\x1b[${body}u`
}

function sgrMouse(rng: Rng): string {
  const button =
    rng.pick([0, 1, 2, 3]) + rng.pick([0, 0, 0, 4, 8, 16, 28]) + rng.pick([0, 0, 32, 64, 64 + 32]) +
    (rng.chance(0.03) ? 128 : 0)
  const coord = () => (rng.chance(0.03) ? "123456789012345678901" : String(rng.int(0, 300)))
  const final = rng.pick(["M", "m"])
  if (rng.chance(0.1)) {
    // malformed
    return rng.pick([
      `\x1b[<${button};${coord()}${final}`,
      `\x1b[<${button};;${coord()}${final}`,
      `\x1b[<${button};${coord()};${coord()};1${final}`,
      `\x1b[<;${coord()};${coord()}${final}`,
      `\x1b[<${button};${coord()};${coord()}X`,
      `\x1b[<${button};${coord()}`,
      `[<${button};${coord()};${coord()}${final}`,
    ])
  }
  return `\x1b[<${button};${coord()};${coord()}${final}`
}

function token(rng: Rng): Uint8Array {
  switch (rng.int(0, 21)) {
    case 0:
    case 1:
      return ascii(Array.from({ length: rng.int(1, 4) }, () => String.fromCharCode(rng.int(0x20, 0x7e))).join(""))
    case 2:
      return Uint8Array.of(rng.pick([0, 1, 3, 8, 9, 10, 13, 26, 27, 28, 29, 30, 31, 127, rng.int(0, 31)]))
    case 3:
      return enc.encode(rng.pick(UTF8_CHARS))
    case 4:
      // invalid or legacy high bytes, truncated sequences
      return Uint8Array.from(
        rng.pick([
          [0x80],
          [0xbf],
          [0xc0, 0x80],
          [0xc1],
          [0xc3],
          [0xe4, 0xb8],
          [0xf0, 0x9f, 0x98],
          [0xf5],
          [0xff],
          [0xed, 0xa0, 0x80],
          [0xe0, 0x80, 0x80],
          [rng.int(0x80, 0xff)],
          [0xc3, 0x41],
          [0xef, 0xbb, 0xbf],
        ]),
      )
    case 5:
      return ascii(rng.pick(["\x1b", "\x1b", "\x1b\x1b"]))
    case 6:
      return ascii(
        "\x1b" +
          rng.pick(["a", "A", "z", "F", "B", "1", "\r", "\n", "\x7f", "\b", " ", "\x01", "\x00", "\x1c", ".", "[", "O", "]", "P", "_", "f", "b"]),
      )
    case 7:
    case 8:
      return ascii((rng.chance(0.1) ? "\x1b\x1b" : "\x1b") + rng.pick(LEGACY))
    case 9:
      return ascii(`\x1b[27;${rng.int(1, 20)};${rng.pick([13, 27, 9, 32, 127, 8, 48, 57, 65, 97, 0x263a, 55296, 70000])}~`)
    case 10:
    case 11:
      return enc.encode(kitty(rng))
    case 12:
    case 13:
      return ascii(sgrMouse(rng))
    case 14:
      return Uint8Array.from([0x1b, 0x5b, 0x4d, rng.int(0, 255), rng.int(0, 255), rng.int(0, 255)])
    case 15: {
      // bracketed paste
      const body: Uint8Array[] = []
      for (let i = rng.int(0, 4); i > 0; i--) {
        body.push(
          rng.pick([
            () => enc.encode(rng.pick(["hello", "line\nbreak", "tab\there", "😀", "\x1b[A", "\x1b[201", "\x1b[20", "\x1b"])),
            () => Uint8Array.from({ length: rng.int(1, 12) }, () => rng.int(0, 255)),
          ])(),
        )
      }
      if (rng.chance(0.85)) body.push(ascii("\x1b[201~"))
      return concat([ascii("\x1b[200~"), ...body])
    }
    case 16:
      return ascii(rng.pick(["\x1b[I", "\x1b[O"]))
    case 17:
    case 18:
      return enc.encode(rng.pick(REPLIES))
    case 19:
      return Uint8Array.from({ length: rng.chance(0.2) ? rng.int(10, 60) : rng.int(1, 6) }, () =>
        rng.pick([rng.int(0, 255), 0x1b, 0x5b, 0x3b, 0x3f, 0x3c, 0x24, 0x3a, rng.int(0x30, 0x39), rng.int(0x40, 0x7e)]),
      )
    case 20:
      return ascii(
        rng.pick([
          "\x1b[",
          "\x1b[1;",
          "\x1b[?",
          "\x1b[?1",
          "\x1b[<",
          "\x1b[4;",
          "\x1b[4;60",
          "\x1b[12;",
          "\x1bO",
          "\x1b]11;",
          "\x1bP",
          "\x1b_G",
          "\x1b[[",
          "\x1b[M",
          "\x1b[ !p",
          "\x1b[1 q",
          "\x1b[97:65;",
          "\x1b[1;5",
        ]),
      )
    default:
      return ascii(rng.pick(["[<35;10;5M", "[<0;1;1m", "[M ab", "[<64;3;3M", "<1;2;3M"]))
  }
}

const GAPS = () => [0, 0, 0, 0, 1, 3, 10, 15, 19, 19.5, 20, 20, 21, 25, 40, 100, 1000]

function program(rng: Rng): Program {
  const p = new Program()
  const timeout = rng.chance(0.85) ? "20" : rng.pick(["-", "5", "50", "0", "-3", "7.9"])
  const max = rng.chance(0.7) ? String(64 * 1024 * 1024) : rng.pick(["-", "8", "16", "40", "200"])
  const arm = rng.chance(0.93) ? "1" : "0"
  const kittyOption = rng.chance(0.7) ? "1" : rng.pick(["0", "-"])
  let bits = ""
  for (let i = 0; i < CONTEXT_FIELDS.length; i++) bits += rng.chance(0.6) ? "-" : rng.chance(0.5) ? "1" : "0"
  if (rng.chance(0.5)) bits = (rng.chance(0.8) ? "1" : "0") + bits.slice(1)
  p.exec(`new ${timeout} ${max} ${arm} ${kittyOption} ${bits}`)

  const tokens = Array.from({ length: rng.int(1, 30) }, () => token(rng))
  const stream = concat(tokens)
  // Cut the stream into reads.
  const cuts = new Set<number>()
  const cutCount = rng.chance(0.2) ? stream.length : rng.int(0, Math.min(stream.length, 12))
  for (let i = 0; i < cutCount; i++) cuts.add(rng.int(1, Math.max(1, stream.length - 1)))
  if (rng.chance(0.15)) {
    // cut at every token boundary
    let offset = 0
    for (const t of tokens) cuts.add((offset += t.length))
  }
  const points = [...cuts].filter((c) => c > 0 && c < stream.length).sort((a, b) => a - b)
  let start = 0
  const gaps = GAPS()
  for (const end of [...points, stream.length]) {
    p.exec(`push ${hex(stream.subarray(start, end))}`)
    start = end
    if (rng.chance(0.5)) p.exec(`adv ${rng.pick(gaps)}`)
    if (rng.chance(0.06)) {
      const field = rng.int(0, CONTEXT_FIELDS.length - 1)
      p.exec(`ctx ${field} ${rng.int(0, 1)} ${rng.int(0, 1)}`)
    }
    if (rng.chance(0.02)) p.exec(rng.pick(["abort", "pause", "resume", "mreset", "pix", "cap", "read", "flush -", "flush 25", "push -"]))
    if (rng.chance(0.005)) p.exec(rng.pick(["reset", "destroy"]))
  }
  p.exec(`adv ${rng.pick([0, 20, 25, 100])}`)
  if (rng.chance(0.3)) p.exec("adv 100")
  p.exec("pix")
  return p
}

function direct(rng: Rng): Program {
  const p = new Program()
  for (let i = rng.int(1, 8); i > 0; i--) {
    switch (rng.int(0, 5)) {
      case 0:
      case 1: {
        const bytes = concat(Array.from({ length: rng.int(1, 2) }, () => token(rng)))
        p.exec(`pk ${hex(bytes)} ${rng.int(0, 1)}`)
        break
      }
      case 2:
        p.exec(`pkb ${rng.int(128, 255)} ${rng.int(0, 1)}`)
        break
      case 3: {
        const chunks = Array.from({ length: rng.int(1, 3) }, () =>
          hex(concat(Array.from({ length: rng.int(1, 4) }, () => (rng.chance(0.8) ? ascii(sgrMouse(rng)) : token(rng))))),
        )
        p.exec(`mall ${chunks.join(" ")}`)
        break
      }
      case 4: {
        const parts = Array.from({ length: rng.int(1, 5) }, () =>
          rng.chance(0.3) ? enc.encode(rng.pick(["\u009b1;2H", "\u0090q\u009c", "\u009d0;t\u0007", "\u0098x", "\u009f", "\x1b(B"])) : token(rng),
        )
        p.exec(`strip ${hex(concat(parts))}`)
        break
      }
      default:
        p.exec(`dec ${hex(concat(Array.from({ length: rng.int(1, 4) }, () => token(rng))))}`)
    }
  }
  return p
}

const programs: string[] = []
const results: string[] = []
const finish = (p: Program) => {
  programs.push(...p.ops, "end")
  results.push(...p.out, "end")
}
for (const ops of handPicked) {
  const p = new Program()
  for (const op of ops) p.exec(op)
  finish(p)
}
const count = Number(arg("count", "20000"))
const seed = Number(arg("seed", "1"))
for (let index = 0; index < count; index++) {
  const rng = new Rng(seed * 1000003 + index)
  finish(index % 5 === 4 ? direct(rng) : program(rng))
}
writeFileSync(arg("programs")!, programs.join("\n") + "\n")
writeFileSync(arg("results")!, results.join("\n") + "\n")
process.stderr.write(`oracle: ${handPicked.length} hand-picked and ${count} random programs, ${programs.length - handPicked.length - count} ops\n`)
